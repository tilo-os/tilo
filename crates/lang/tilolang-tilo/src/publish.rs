//! Publishing tilolang resources on the action api.
//!
//! Every tilolang resource in the device's store is published as a `tilo_actions::ResourceSpec`
//! of kind `tl.<app>.<Type>`, with one typed action per tilolang action whose target is its type.
//! The desktop agent, the runtime api, shortcuts and grants therefore reach tilolang actions with
//! no agent-side code. The build closure of each action runs the wasm **planner** and encodes
//! the resulting [`DynAction`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value as Json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, ActionType, EncodedAction, ExecutionLocus, Noun,
    ParamType, ResourceSpec, ResourceType, ResourceViewSpec, Target,
};

use crate::action::{BLOCK_EDITS, BlockEdit, BlockEditKind, DIRECT_EDITS, DirectEdit, DynAction};
use crate::resources::{ResourceStore, Watch};
use tilo_actions::api::{PlacedAction, survey_placed_level};
use tilo_resource::Effect as _;
use tilolang_runtime::ids::Actor;
use tilolang_runtime::meta::{ActionMeta, FieldKind, Meta, Ty, TypeMeta};
use tilolang_runtime::store::{
    ANY_FIELD, BytesData, ChangeNote, FieldSpec, FieldValue, Listing, Mutation, Scalar, Store as _,
};
use tilolang_runtime::value::ResourceRef;
use tilolang_runtime::words::Words;

use crate::meta::{MetaExt as _, action_kind};
use crate::naming::{HOST_RESOURCE, app_name, package_resource_id, resource_id};
use crate::surface::{
    Merge, editable_field_names, is_destructive, published_param, rich_field_names,
};

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

/// Who is invoking: what the API admitted of the caller, for the runtime's actor.
///
/// An agent is a task of a person under that person's narrowed rights, and the API tells the
/// host which task (the delegate's id) and for whom. A person at the window is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The authenticated actor: a person, or an agent task's delegate.
    pub actor: tilolang_runtime::ids::Actor,
    /// Whether the actor is software acting for a person.
    pub agent: bool,
    /// The person an agent acts for.
    pub on_behalf_of: Option<String>,
    /// Whether that person's own device handed the agent its session, and answers for what it
    /// does: an agent that is not handed acts under a key of its own.
    pub handed: bool,
    /// The app whose process the call came through, as the API admitted the connection: what the
    /// action reads as `invoker.app()`.
    pub app: Option<String>,
}

impl Caller {
    /// Whose read rights bound what the caller reads: the person an agent acts for, else the
    /// caller.
    #[must_use]
    pub fn reader(&self) -> tilolang_runtime::ids::Actor {
        self.on_behalf_of.as_deref().map_or_else(
            || self.actor.clone(),
            |person| {
                tilolang_runtime::ids::Actor::Member(tilolang_runtime::ids::MemberId::person(
                    person,
                ))
            },
        )
    }
}

impl Caller {
    /// How the caller acts, when software acts for a person: for whom, and whether that person's
    /// device handed it its session. `None` for a caller acting for nobody, or for a person by
    /// an id that is none.
    #[must_use]
    pub fn acting_for(&self) -> Option<tilo_actions::Acting> {
        let person = tilo_actions::PersonId::parse(self.on_behalf_of.as_deref()?).ok()?;
        let by = tilo_actions::Acting::from(self.actor.clone());
        let by = if self.handed { by.handed() } else { by };
        Some(by.acting_for(tilo_actions::Acting::person(person)))
    }

    /// On whose behalf the caller invokes, as the app reads `invoker`: the actor, or the person
    /// an automation runs for ([`tilo_actions::Acting::principal`]).
    #[must_use]
    pub fn principal(&self) -> tilolang_runtime::ids::Actor {
        self.acting_for()
            .map_or_else(|| self.actor.clone(), |acting| acting.principal().clone())
    }
}

impl From<&tilo_actions::ActionInvoker> for Caller {
    /// What the API admitted of `invoker`, as the runtime reads it.
    fn from(invoker: &tilo_actions::ActionInvoker) -> Self {
        Self {
            actor: invoker.acting.by.clone(),
            agent: !invoker.acting.is_person(),
            on_behalf_of: invoker.acting.acted_for_person().map(str::to_owned),
            handed: invoker.acting.handed,
            app: invoker.acting.app().map(|app| app.as_str().to_owned()),
        }
    }
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
    /// The target resource's id, when the action has one.
    pub resource: Option<&'a str>,
    /// The arguments beyond the target, by parameter name.
    pub args: &'a serde_json::Map<String, Json>,
    /// Who invokes, when it is not the person at the window.
    pub caller: Option<&'a Caller>,
    /// The resource the invocation is made from, when it is made from one: what the resources
    /// it creates were made from (`created(r)?.from`, `linked(r)`). A "make from this" menu,
    /// send-to, or an agent working from what it read sets it.
    pub from: Option<&'a ResourceRef>,
}

/// Runs a tilolang action as a planner and records what was invoked.
///
/// The implementation is the host's runtime: the action runs against the device's store, as a
/// trial run taken back afterwards unless the invocation is being carried out
/// ([`crate::host::Host::carrying_out`]).
pub trait Planner: Send + Sync {
    /// # Errors
    /// Why the action cannot run: a trap, a refused argument, an unknown action.
    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String>;

    /// The caller of the invocation being built now, as the API admitted it.
    fn caller(&self) -> Option<Caller> {
        None
    }
}

/// The app whose published resource `resource` is: a document's or item's (`tl:<app>:<id>`,
/// [`resource_id`]) or its package's own (`tl:<app>`, [`package_resource_id`]).
#[must_use]
pub fn app_of(resource: &str) -> Option<&str> {
    let rest = resource.strip_prefix("tl:")?;
    Some(rest.split_once(':').map_or(rest, |(app, _)| app))
}

/// The app and the path in it of the published id `resource` (`tl:<app>:<path>`).
#[must_use]
pub fn split_published(resource: &str) -> Option<(&str, &str)> {
    resource.strip_prefix("tl:")?.split_once(':')
}

/// What a resource of `schema` in the package `meta` is called while none of its own text names
/// it: what its type declares (`(untitled: "Untitled")`, in the reader's words), else a
/// package's root by the app's name ("Notes", "Notizen"), anything else by its type ("Todo
/// list").
#[must_use]
pub fn untitled(meta: &Meta, schema: &crate::schema::TypeSchema, words: &dyn Words) -> String {
    meta.types
        .iter()
        .find(|declared| declared.id == schema.id)
        .map_or_else(
            || {
                if meta.roots.contains(&schema.id) {
                    app_name(words, meta)
                } else {
                    schema.untitled()
                }
            },
            |declared| untitled_of(meta, declared, words),
        )
}

/// What a document of `declared` is called while its title is empty: the words its type declares,
/// translated; else an app's root by the app's name, translated; else its type's name.
fn untitled_of(meta: &Meta, declared: &tilolang::meta::TypeMeta, words: &dyn Words) -> String {
    if let Some(untitled) = &declared.untitled {
        return said_of(words, &declared.id, "untitled", None, untitled);
    }
    if meta.roots.contains(&declared.id) {
        app_name(words, meta)
    } else {
        crate::schema::spoken(&declared.name)
    }
}

/// The type of the typed action `action` on resources of kind `kind`, whose build runs
/// `planner` on the target its spec is bound to: the resource id it is published on, a
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
    // A row of one of the action's target types addresses its resource: the first parameter is
    // what the row is, not something the caller gives. The package's own row, where an action
    // taking a union of types is offered, takes it as a reference argument instead.
    let targeted = meta
        .targets_of(&action.params)
        .iter()
        .any(|target| meta.kind_of(&target.name) == kind);
    let params = published_params(meta, action, words, targeted);
    let build = build_of(meta, kind, action, planner);
    let (label, placeholder) = naming_of(action, params.len(), words);
    let spec = spec_of(meta, action, label, placeholder, params, build);
    // The kind an action's `search:` answers with, as its app publishes the type.
    let search = action.search.as_ref().map(|target| meta.kind_of(target));
    Arc::clone(described(meta, spec, action, search, words).action_type())
}

/// The schema of the value the action `action` of `meta` returns, in the API's JSON: what an
/// invocation of it is held to, so a caller reads the value as the data it declared. `None` for
/// an action declared to return nothing, and for one whose value the API carries no data for (the
/// bytes of a response): that app shows it rather than yielding it.
///
/// A value may always be absent: an invocation that stopped for a person to decide first has not
/// run, and returns nothing until they do.
#[must_use]
pub fn result_schema(meta: &Meta, action: &ActionMeta) -> Option<Json> {
    let ty = result_ty(meta, action)?;
    let inner = match &ty {
        Ty::Option(inner) => inner.as_ref(),
        other => other,
    };
    let schema = match inner {
        Ty::File => crate::surface::file_schema(),
        Ty::List(item) if **item == Ty::File => {
            serde_json::json!({"type": "array", "items": crate::surface::file_schema()})
        }
        other => crate::surface::schema_of(meta, other),
    };
    Some(serde_json::json!({"oneOf": [schema, {"type": "null"}]}))
}

/// The type the action `action` of `meta` returns, when the API carries its value as data
/// ([`carries_data`]).
pub(crate) fn result_ty(meta: &Meta, action: &ActionMeta) -> Option<Ty> {
    let ty = tilolang_runtime::meta::parse_ty(action.result.as_deref()?, &meta.app).ok()?;
    carries_data(&ty).then_some(ty)
}

/// Whether a value of `ty` is one the API carries as data: a text, a number, a flag, a person,
/// rich text, a resource by the reference to it, a file by the reference to its content, a list
/// of them, an option of one. The bytes of a response, an action and a view are the app's own: no
/// caller reads one.
fn carries_data(ty: &Ty) -> bool {
    match ty {
        Ty::File
        | Ty::Text
        | Ty::Int
        | Ty::Float
        | Ty::Bool
        | Ty::RichText
        | Ty::Date
        | Ty::Time
        | Ty::Identifier
        | Ty::Actor
        | Ty::Json
        | Ty::Record(_)
        | Ty::Union(_)
        | Ty::Enum(_) => true,
        Ty::Option(inner)
        | Ty::List(inner)
        | Ty::Set(inner)
        | Ty::Collection(inner)
        | Ty::Map(inner) => carries_data(inner),
        Ty::Tuple(items) => items.iter().all(carries_data),
        _ => false,
    }
}

/// The action's parameters, each with what it is called where a person gives it (a form, a
/// consent sheet) in the reader's language.
fn published_params(
    meta: &Meta,
    action: &ActionMeta,
    words: &dyn Words,
    targeted: bool,
) -> Vec<ActionParam> {
    let declared = meta.arguments(&action.params);
    // A union's target is still among the arguments, its type naming no one record to skip.
    let declared = if targeted && meta.target_of(&action.params).is_none() {
        declared.get(1..).unwrap_or_default()
    } else {
        declared
    };
    declared
        .iter()
        .map(|param| {
            let key = tilolang::messages::param_key(&action.id, &param.name);
            published_param(meta, param).titled(words.word(&key, &[], &param.words()))
        })
        .collect()
}

/// The build of the action's type: it runs `planner` on the target its spec is bound to, as the
/// caller named the invocation with the arguments, and encodes what it planned.
fn build_of(
    meta: &Meta,
    kind: &str,
    action: &ActionMeta,
    planner: &Arc<dyn Planner>,
) -> impl Fn(&Target, &ActionArgs) -> EncodedAction + Send + Sync + 'static {
    let (app, kind, id) = (meta.app.clone(), kind.to_owned(), action.id.clone());
    let planner = Arc::clone(planner);
    move |target: &Target, args: &ActionArgs| {
        let json = args.to_json();
        let caller = planner.caller();
        // What the invocation is made from, as the caller named it with the arguments.
        let from = args.from().and_then(|from| {
            // The kind names the type; one no loaded package declares is read off the
            // resource where it is stamped.
            let ty = crate::resources::kinds::type_of_kind(&from.kind).unwrap_or_default();
            ResourceRef::parse_id(&from.resource, ty)
        });
        let request = PlanRequest {
            app: &app,
            kind: &kind,
            action_id: &id,
            resource: target.get::<String>().map(String::as_str),
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
    }
}

/// What the action is called and what its form asks for, in the reader's language. `given` is
/// how many parameters it publishes: the rest of the declared ones are the form's.
fn naming_of(action: &ActionMeta, given: usize, words: &dyn Words) -> (String, String) {
    // What the platform calls the action, as the compiler derived it, in the reader's words.
    let mut label = label_of(
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
    // not ask for — the target, the resource it runs against — and the parameters after it are
    // the form's. The form's own placeholder names the command without the dots, which ask.
    if action
        .unbound(action.params.len().saturating_sub(given))
        .next()
        .is_some()
        && !label.ends_with('…')
    {
        label.push('…');
    }
    (label, placeholder)
}

/// The action's spec, under the name its declaration keeps: planned when it runs when it takes
/// no parameters, else typed with the form its parameters make.
fn spec_of(
    meta: &Meta,
    action: &ActionMeta,
    label: String,
    placeholder: String,
    params: Vec<ActionParam>,
    build: impl Fn(&Target, &ActionArgs) -> EncodedAction + Send + Sync + 'static,
) -> ActionSpec {
    // An action without parameters is planned when it runs, not when it is published: planning
    // is a trial run of the action, and publishing every item's actions must not run them all.
    let no_params = params.is_empty();
    // Published by its name, as a native verb is: what an agent at the API lists and calls. The
    // build above plans it by the id the declaration keeps, which is also what a change of it
    // says in the document (and its words are keyed by).
    let published = crate::meta::published_name(action).to_owned();
    let spec = if no_params {
        ActionSpec::deferred_on(
            published,
            label,
            ExecutionLocus::ClientLocal,
            crate::action::ACTION_TYPE,
            move |target| build(target, &ActionArgs::none()),
        )
    } else {
        ActionSpec::typed_on(
            published,
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
    if defaulted && !no_params {
        spec.with_immediate_per_target()
    } else {
        spec
    }
}

/// `spec` made into its type in the order a native verb's declaration is made into it: its kind,
/// description, icon, then whether it is destructive, which settles its confirmation.
fn described(
    meta: &Meta,
    spec: ActionSpec,
    action: &ActionMeta,
    search: Option<String>,
    words: &dyn Words,
) -> ActionSpec {
    let mut spec = spec.of_kind(action_kind(action.class));
    if !action.doc.is_empty() {
        spec = spec.with_description(said_of(words, &action.id, "doc", None, &action.doc));
    }
    if let Some(icon) = &action.icon {
        spec = spec.with_icon(icon.clone());
    }
    // `did: "added a subtask"`: what it did, said after who did it, in the reader's language, as
    // history says it; a document of an agent's steps reads the same words.
    if let Some(did) = &action.did {
        spec = spec.with_did(said_of(words, &action.id, "did", None, did));
    }
    // Other words the system's search finds it by, in the reader's language.
    if !action.keywords.is_empty() {
        let said = said_of(
            words,
            &action.id,
            "keywords",
            None,
            &action.keywords.join(", "),
        );
        spec = spec.with_keywords(
            said.split(',')
                .map(str::trim)
                .filter(|word| !word.is_empty()),
        );
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
    // What it asks the service for: the capabilities its body's `outside(...)` calls name, so a
    // query naming a kind of an outside service finds the app's own action that asks its origin
    // (`outside:youtube.search`). The tag is the compiler's reading of the code, never a name a
    // host invented.
    {
        let contract = &mut spec.type_mut().contract;
        for capability in &action.outside {
            let tag = format!("{}{capability}", tilo_actions::OUTSIDE_CAPABILITY_PREFIX);
            if !contract.tags.contains(&tag) {
                contract.tags.push(tag);
            }
        }
        // The app's search over its remote service (`search: Found`): a query naming its kind
        // asks this action, and the records of the kind it writes are what the search places.
        if let Some(kind) = search {
            for tag in [
                tilo_actions::SEARCH_TAG.to_owned(),
                format!("{}{kind}", tilo_actions::SEARCH_KIND_PREFIX),
            ] {
                if !contract.tags.contains(&tag) {
                    contract.tags.push(tag);
                }
            }
        }
    }
    // What it returns: the shape an invocation of it is held to, so a caller reads the value as
    // the data the declaration says it returns (`-> Note` as the resource it made).
    if let Some(schema) = result_schema(meta, action) {
        spec.type_mut().contract.result = tilo_actions::ResultSpec::Json { schema };
    }
    // `shows: [(d.to, .addresses), d.subject]`: what a person asked to allow it reads first, in
    // the declaration's order: the arguments its `shows:` names, which admission fills the ask's
    // facts from. What a `fn` of the app answers is read by the app's own run where it asks.
    spec.type_mut().contract.shows = action
        .shows
        .iter()
        .filter(|shown| shown.call.is_none())
        .map(shown_of)
        .collect();
    // What the person types where they answer its ask, labelled in their language; the action
    // reads it from their approve.
    spec.type_mut().contract.takes = action
        .takes
        .iter()
        .enumerate()
        .map(|(n, take)| tilo_actions::remote::asked::Take {
            label: said_of(
                words,
                &action.id,
                &tilolang::meta::take_key(n.saturating_add(1)),
                None,
                &take.label,
            ),
            secret: take.secret,
            shown: false,
            initial: None,
            chord: false,
        })
        .collect();
    // A check its project's host approved runs as a job its invoker follows, and what it
    // answers is what the commands printed, which no one vetted: the contract the broker runs a
    // check for. It declares no cancellation of its own: while it waits on the device's check,
    // a step that can be stopped, the dispatch lets its invoker stop it, as it does any action.
    if crate::meta::runs_approved_check(action) {
        let contract = &mut spec.type_mut().contract;
        contract.execution = tilo_actions::ExecutionMode::Background { cancellable: false };
        if matches!(contract.result, tilo_actions::ResultSpec::None) {
            // What it answers is the host's receipt of what the commands did: a document with
            // the outcome, the exit code and the output they printed.
            contract.result = tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({ "type": "object" }),
            };
        }
        let untrusted = tilo_actions::UNTRUSTED_CONTENT_TAG.to_owned();
        if !contract.tags.contains(&untrusted) {
            contract.tags.push(untrusted);
        }
        // The device runs the check as this action's own step: the yes the action was given
        // covers it.
        contract.effects.push(tilo_actions::ActionEffect::new(
            tilo_action_services::approved_checks::PROVIDER,
            tilo_action_services::approved_checks::RESOURCE,
            tilo_action_services::approved_checks::RUN,
        ));
    }
    confirmation(&mut spec.type_mut().contract, action);
    spec
}

/// What `action`'s calls confirm: what leaves the system confirms every call, as it does every
/// press of it ([`ActionMeta::confirms`]), and says so ([`tilo_actions::LEAVES_TAG`]), so a window
/// or the shell deciding a press of it from its contract decides it as the runtime does. A
/// destructive one confirms where undo cannot take it back, as a native verb does; a press of it
/// confirms all the same unless Shift is held.
fn confirmation(contract: &mut tilo_actions::ActionContract, action: &ActionMeta) {
    if action.leaves && !contract.leaves() {
        contract.tags.push(tilo_actions::LEAVES_TAG.to_owned());
    }
    if action.confirms() == tilolang::meta::Confirms::Always {
        contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    } else {
        contract.settle_confirmation();
    }
}

/// One entry of an action's `shows:` as its contract publishes it.
fn shown_of(shown: &tilolang::meta::ShownMeta) -> tilo_actions::remote::asked::Shown {
    use tilo_actions::remote::asked::ShownAs;
    tilo_actions::remote::asked::Shown {
        param: shown.param.clone(),
        field: shown.field.clone(),
        render: match shown.render.as_deref() {
            Some("addresses") => ShownAs::Addresses,
            Some("long") => ShownAs::Long,
            Some("secret") => ShownAs::Secret,
            _ => ShownAs::Text,
        },
    }
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
    /// Whether any of the package's types holds what an outside service sent (`origin:
    /// imap.message`): what its resources read back was written by people outside the workspace,
    /// so each read action says it carries untrusted content.
    outside_origin: bool,
}

/// The action types of the resources of one tilolang type.
struct TypeActions {
    /// The direct edits ([`edit_types`]), which every document of the type offers, root and
    /// item alike: its text and flag fields, and one paragraph of its rich text.
    edits: Vec<Arc<ActionType>>,
    /// The actions whose target is the type.
    actions: Vec<Arc<ActionType>>,
    /// The views that show a resource of the type, each with the action that opens it through
    /// that view, when that is not the resource's own `open` ([`views_of`]).
    views: Vec<(ResourceViewSpec, Option<Arc<ActionType>>)>,
    /// The places a window of a root of the type can be opened at, each with the action that
    /// opens it there ([`places_of`]), by the place's id.
    places: Vec<(Arc<ActionType>, String)>,
    /// What its resources are, as people name them: its noun, its app's name, and whether
    /// it is a root (each resource published with it says so, [`ResourceType`]).
    named: ResourceType,
    /// What one of its resources is called while its title is empty ([`untitled_of`]).
    untitled: String,
}

impl ActionTypes {
    /// What a resource of the type `type_name` is called while no text in it names it: the words
    /// its type declares, else a package's root (one per workspace) by the package's title,
    /// another by its type. Never by its id.
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
                    // An action whose first parameter takes a union of types acts on a resource
                    // of any of them (`send(to: TextChannel | Thread | Direct)`), and is
                    // published on each.
                    meta.targets_of(&action.params)
                        .iter()
                        .any(|target| target.name == declared.name)
                })
                .map(|action| action_type(meta, &kind, action, planner, words))
                .collect();
            on_type.insert(
                declared.name.clone(),
                TypeActions {
                    edits: edit_types(meta, &declared.name, planner),
                    actions,
                    views: views_of(meta, &declared.name, words),
                    places: places_of(meta, declared, words),
                    named: ResourceType {
                        // What one and several of the type are called, by count: the catalogue
                        // refines a noun English cannot derive ("notebooks", not "noteses").
                        noun: Noun::new(
                            said_of(words, &declared.id, "noun", Some("1"), &declared.noun()),
                            said_of(words, &declared.id, "noun", Some("2"), &declared.plural()),
                        ),
                        // The app's name in the reader's words, and its title in the package's
                        // own, which a search in the source language still matches.
                        app: Noun::new(
                            app_name(words, meta),
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
            outside_origin: meta.types.iter().any(|ty| ty.origin.is_some()),
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

    /// The document `id` of the type `type_name`, labelled `label`, carrying its `open`, its
    /// `read`, the direct edits of its type ([`edit_types`]) and the actions on its type. A root
    /// and an item are published alike: an item's edits are the same typed field edits, written to
    /// the item ([`edit_action`]).
    ///
    /// A `root` document's `open` also takes the item under it to show, which a window that shows
    /// none of the document's items does not publish: a search hit on such an item opens it there.
    fn resource(&self, type_name: &str, id: &str, label: String, root: bool) -> ResourceSpec {
        let open = if root {
            root_open_spec(id)
        } else {
            open_spec(id)
        };
        let published = resource_id(&self.meta.app, id);
        // What it holds is the workspace's content, read as data: an agent reads a note's text
        // rather than a screenshot of it. A type that holds what an outside service sent reads
        // back words its person did not write, and the action says so: a model that reads them is
        // told they are data to weigh, never instructions.
        let mut read = tilo_resource::read::spec(Some(&published));
        if self.outside_origin {
            read.type_mut()
                .contract
                .tags
                .push(tilo_actions::UNTRUSTED_CONTENT_TAG.to_owned());
        }
        let mut resource = ResourceSpec::new(published, self.meta.kind_of(type_name), label)
            .with_action(open)
            .with_action(read);
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
        for (opens, place) in &of_type.places {
            resource = resource.with_action(open_with_spec(opens, id, place));
        }
        // The link to it, as the system's own verbs bind it to every resource with a view: the
        // palette, the context menu and an agent ask for "copy link" the same way they ask an
        // app's own actions, and the host mints it ([`crate::share`]).
        if root {
            tilo_resource::share::offer_resource_link(&mut resource);
            // The link into it: the root of an app that opens links publishes the system's
            // `open-link`, bound to itself, so a shell hands the app a link of a scheme it claims
            // however the app reached it ([`crate::share`]'s neighbour).
            offer_open_link(&self.meta, &mut resource);
        } else {
            tilo_resource::share::offer_link(&mut resource);
        }
        let target = Target::new(id.to_owned());
        for ty in of_type.edits.iter().chain(&of_type.actions) {
            resource = resource.with_action(ty.bind(target.clone()));
        }
        resource
    }
}

/// Offer the system's `open-link` on a root whose package declares `open_link`, unless it offers
/// it already: the shared view action a shell delivers a link through, which the host answers by
/// running the app's own `open_link` with it, so a link reaches the app whether or not it is
/// open and whatever route it took. The declaration stays published under its own name beside
/// it, since a change it makes is claimed by that action ([`crate::surface`]), and an agent's
/// types describe it.
fn offer_open_link(meta: &Meta, resource: &mut tilo_actions::ResourceSpec) {
    let Some(root) = crate::meta::open_link_root(meta) else {
        return;
    };
    if resource.kind != meta.kind_of(&root.name)
        || resource
            .actions
            .iter()
            .any(|spec| spec.id == collab_types::app_actions::OPEN_LINK)
    {
        return;
    }
    let target = Target::new(resource.id.as_str().to_owned());
    let spec = collab_types::app_actions::open_link_spec();
    resource.actions.push(spec.action_type().bind(target));
}

/// The views a resource of `type_name` is shown through: the views a declaration attaches to its
/// type ([`Meta::views_showing`]), each taking the resource and nothing else, so opening one needs
/// only the resource (`open(r, with: v)`).
///
/// A view and the action that opens it name each other one to one: the API projects a view
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
    let placed = meta.place_view(&declared.id);
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
            let is_place = placed == Some(view.id.as_str());
            let opens = (!is_primary)
                .then(|| open_with_type(&view.id, &name, &label, description.as_deref(), is_place));
            let open_action = opens.as_ref().map_or("open", |opens| opens.id.as_str());
            let mut spec = ResourceViewSpec::new(view.id.clone(), label, open_action);
            if let Some(line) = description {
                spec = spec.with_description(line);
            }
            (spec.with_primary(is_primary).with_place(is_place), opens)
        })
        .collect()
}

/// The places a window of `declared` can be opened at (`on show week: ...` in its default view,
/// for a root or for what a window opens, such as one comparison), each with the action that opens the window there: an
/// [`OpenEffect::OpenWith`](crate::effects::OpenEffect::OpenWith) naming the place, labelled in
/// the reader's words, which the launcher, the palette, an agent and a script invoke alike. The
/// window answers it by taking its own view to the place, for the person who opened it: what
/// another person's window shows, and the workspace, stay as they were.
fn places_of(
    meta: &Meta,
    declared: &tilolang::meta::TypeMeta,
    words: &dyn Words,
) -> Vec<(Arc<ActionType>, String)> {
    meta.places_of(&declared.id)
        .iter()
        .map(|place| {
            let mut spec = crate::effects::OpenEffect::OpenWith {
                anchor: String::new(),
                view: place.id.clone(),
            }
            .bound();
            let ty = spec.type_mut();
            ty.id = show_place_id(&place.name);
            ty.label = label_of(words, &place.id, &place.label);
            ty.description = None;
            (Arc::clone(spec.action_type()), place.id.clone())
        })
        .collect()
}

/// The id of the action that opens a window at its place named `place`.
#[must_use]
pub fn show_place_id(place: &str) -> String {
    format!("show-{place}")
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
///
/// A type's place view (`place`) is what a place shows the resource through on its own,
/// never what a person turns the app's window into: the canvas and an agent's `place` stand it,
/// so its open action is offered to agents and kept out of people's palette and menus, where it
/// would replace the window's sidebar and bar with the bare view.
fn open_with_type(
    view: &str,
    name: &str,
    label: &str,
    description: Option<&str>,
    place: bool,
) -> Arc<ActionType> {
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
    if place {
        ty.contract.set_audience(tilo_actions::Audience::Agents);
    }
    Arc::clone(spec.action_type())
}

/// The action that opens the published resource `anchor` through the view `view`, of the type
/// [`open_with_type`] built for that view: bound as [`tilo_resource::Effect::bound`] binds an
/// effect to the resource it is offered on.
fn open_with_spec(ty: &Arc<ActionType>, anchor: &str, view: &str) -> ActionSpec {
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
pub fn publish_resource(
    types: &ActionTypes,
    type_name: &str,
    id: &str,
    label: &str,
) -> ResourceSpec {
    identified(types.resource(type_name, id, label.to_owned(), true))
}

/// `resource` with what it is, whatever alias a window publishes it under: the kind it publishes
/// as and the id it keeps. A host that approves work on one resource, a caller that names it from
/// another device, and a step an action on it declares bind to exactly it by this.
fn identified(mut resource: ResourceSpec) -> ResourceSpec {
    resource.semantic_identity =
        tilo_actions::ResourceSemanticIdentity::new(resource.kind.clone(), resource.id.as_str());
    resource
}

/// The `open` view action of the published resource `anchor`: what a search hit invokes.
fn open_spec(anchor: &str) -> ActionSpec {
    crate::effects::OpenEffect::Open {
        anchor: anchor.to_owned(),
    }
    .bound()
}

/// The `open` of the root document `anchor`: it opens the document, or the item under it a caller
/// names (`anchor`, as a search hit on the item does). A person choosing it asks for nothing: left
/// out, the item is the document itself.
fn root_open_spec(anchor: &str) -> ActionSpec {
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

/// The resource of a root document with the items of its collections as child resources, each
/// carrying the actions whose target is its type (a draft's `send`, a note's `pin`), read from
/// `store`.
///
/// An item's resource id is its id in the store (`doc/item`), the same form an action takes an
/// item as an argument in, so an agent that surveys the tree calls an item's verbs directly.
#[must_use]
pub fn publish_tree(
    store: &ResourceStore,
    types: &ActionTypes,
    type_name: &str,
    doc: &str,
    label: &str,
) -> ResourceSpec {
    let mut resource = publish_resource(types, type_name, doc, label);
    let Some(declared) = types.meta().type_named(type_name) else {
        return resource;
    };
    let reader = Reader { store, types };
    let at = ResourceRef::root(doc, declared.id.clone());
    for child in reader.items(&at, None) {
        resource = resource.with_child(child);
    }
    resource
}

/// What names the resource `at` in a published tree: the first line of the text its type names
/// its resources by, when it holds one.
fn named(store: &ResourceStore, at: &ResourceRef) -> Option<String> {
    let text = store.first_text(at)?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The collections of the type `ty`, each with the types of its items: in the order `meta`
/// declares the type's fields when it declares the type, which is the order a resource's
/// children are published in.
fn collections_of(
    store: &ResourceStore,
    meta: Option<&Meta>,
    ty: &str,
) -> Vec<(String, Vec<String>)> {
    let kinds = store.kinds();
    let Some(entry) = kinds.get(ty) else {
        return Vec::new();
    };
    let declared = meta.and_then(|meta| meta.types.iter().find(|declared| declared.id == ty));
    let order: Vec<&str> = declared.map_or_else(
        || entry.fields.keys().map(String::as_str).collect(),
        |declared| {
            declared
                .fields
                .iter()
                .map(|field| field.id.as_str())
                .collect()
        },
    );
    order
        .into_iter()
        .filter_map(|id| {
            let field = entry.fields.get(id)?;
            if field.kind != FieldKind::Collection {
                return None;
            }
            (!field.items.is_empty()).then(|| (id.to_owned(), field.items.clone()))
        })
        .collect()
}

/// The item `id` of a collection of `at` holding `items`: of its one type, or, in a collection
/// of several (`Collection<Category | TextChannel>`), of the type the store holds it as.
pub(crate) fn item_of(
    store: &ResourceStore,
    at: &ResourceRef,
    id: &str,
    items: &[String],
) -> Option<ResourceRef> {
    if let [ty] = items {
        Some(at.item(id, ty.clone()))
    } else {
        let ty = store.type_of_id(&at.item(id, String::new()).id_string())?;
        Some(at.item(id, ty))
    }
}

/// Reads what a document publishes from the store: the document, and its items down to what a
/// window reaches of them, or all of them.
struct Reader<'a> {
    store: &'a ResourceStore,
    types: &'a ActionTypes,
}

impl Reader<'_> {
    /// The document `doc`, with what `reached` reaches of it, or every item when `None`. `None`
    /// when the store holds no such document, or no loaded type is its.
    fn document(&self, doc: &str, reached: Option<&Reached>) -> Option<ResourceSpec> {
        let (at, mut tree) = self.document_alone(doc)?;
        for child in self.items(&at, reached) {
            tree = tree.with_child(child);
        }
        Some(tree)
    }

    /// The document `doc` published on its own, without its items, and where it is.
    fn document_alone(&self, doc: &str) -> Option<(ResourceRef, ResourceSpec)> {
        let ty = self.store.type_of_id(doc)?;
        let name = self.store.kinds().get(&ty)?.name.clone();
        let at = ResourceRef::root(doc, ty);
        let label = self.label(&at, &name);
        let tree = publish_resource(self.types, &name, doc, &label);
        Some((at, tree))
    }

    /// What the resource `at` of the type `type_name` is called: its text, else what its
    /// package calls one of its type.
    fn label(&self, at: &ResourceRef, type_name: &str) -> String {
        named(self.store, at).unwrap_or_else(|| self.types.untitled_of(type_name))
    }

    /// The items of the collections of `at`, each with its own items: those `reached` reaches,
    /// read by their ids rather than by listing ten thousand, or every one when `None`.
    fn items(&self, at: &ResourceRef, reached: Option<&Reached>) -> Vec<ResourceSpec> {
        if let Some(reached) = reached {
            return reached
                .get(&at.id_string())
                .into_iter()
                .flatten()
                .filter_map(|child| {
                    let ty = self.store.type_of_id(child)?;
                    self.item(&ResourceRef::parse_id(child, ty)?, Some(reached))
                })
                .collect();
        }
        let mut out = Vec::new();
        for (listing, items) in self.collections(at) {
            for id in listing.iter() {
                if let Some(item) = item_of(self.store, at, &id, &items) {
                    out.extend(self.item(&item, None));
                }
            }
        }
        out
    }

    /// The collections of `at` its tree publishes the items of, in the order it publishes them,
    /// each with its listing and the types of its items.
    fn collections(&self, at: &ResourceRef) -> Vec<(Rc<Listing>, Vec<String>)> {
        collections_of(self.store, Some(self.types.meta()), &at.ty)
            .into_iter()
            .filter_map(|(field, items)| {
                let spec = FieldSpec {
                    id: field,
                    kind: FieldKind::Collection,
                };
                Some((self.store.listing(at, &spec).ok()?, items))
            })
            .collect()
    }

    /// Whether `at` holds an item its tree publishes: whether its resource has children, found
    /// by the first of them rather than by publishing them.
    fn holds_any(&self, at: &ResourceRef) -> bool {
        self.collections(at).iter().any(|(listing, items)| {
            listing.iter().any(|id| {
                item_of(self.store, at, &id, items)
                    .is_some_and(|item| self.store.kinds().get(&item.ty).is_some())
            })
        })
    }

    /// The item `at`, published with its type's direct edits and actions, and its own items.
    fn item(&self, at: &ResourceRef, reached: Option<&Reached>) -> Option<ResourceSpec> {
        let mut resource = self.item_alone(at)?;
        for child in self.items(at, reached) {
            resource = resource.with_child(child);
        }
        Some(resource)
    }

    /// The item `at` published on its own, without its items.
    fn item_alone(&self, at: &ResourceRef) -> Option<ResourceSpec> {
        let name = self.store.kinds().get(&at.ty)?.name.clone();
        let label = self.label(at, &name);
        Some(identified(self.types.resource(
            &name,
            &at.id_string(),
            label,
            false,
        )))
    }
}

/// One level of what a workspace publishes ([`Publications::level`]): the resources directly
/// under one, or the documents at the top, each published only when a page lists it.
pub struct Level {
    /// The published resource the level is under, empty at the top.
    parent: String,
    /// What the level holds, in the order the whole publication holds it.
    runs: Vec<Run>,
}

/// A stretch of a level's resources.
enum Run {
    /// A document at the top, published with its package's action types.
    Document {
        doc: String,
        types: Arc<ActionTypes>,
    },
    /// The items of one collection of `holder`, of the types `items`, published with the action
    /// types of their document's package.
    Items {
        holder: ResourceRef,
        listing: Rc<Listing>,
        items: Vec<String>,
        types: Arc<ActionTypes>,
    },
    /// A resource published beside the level's own ([`Level::then`]).
    Given(Box<ResourceSpec>),
}

impl Level {
    /// The published resource the level is under, empty at the top.
    #[must_use]
    pub fn parent(&self) -> &str {
        &self.parent
    }

    /// Add `resource` after the level's own resources: one its publisher holds beside them, as
    /// a record holds its package's own resource beside its documents.
    pub fn then(&mut self, resource: ResourceSpec) {
        self.runs.push(Run::Given(Box::new(resource)));
    }

    /// The rows `admit` admits of the level, from the `offset`th it admits on: at most `size`
    /// of them, and whether more follow. A row is an action of a resource of the level, as
    /// [`tilo_actions::api::survey_placed`] lists it in the whole publication: in the same order,
    /// under the level's parent, and saying whether its own resource holds more.
    ///
    /// With `every`, `admit` admits every row: the rows before `offset` are then counted by the
    /// resources holding them and never published, since every resource of one type publishes
    /// the same actions ([`ActionTypes::resource`]), and a page costs its own rows wherever it
    /// starts. Without it, each resource of the level from its start is published to put its
    /// rows to `admit`: a page costs its level, never the rest of the workspace.
    pub fn page<R>(
        &self,
        host: &crate::host::Host,
        (offset, size): (usize, usize),
        every: bool,
        admit: &mut dyn FnMut(PlacedAction) -> Option<R>,
    ) -> (Vec<R>, bool) {
        let mut rows = Vec::new();
        let mut more = false;
        let mut admitted = 0usize;
        self.walk(host, if every { offset } else { 0 }, &mut |at, placed| {
            let Some(row) = admit(placed) else {
                return true;
            };
            let index = if every { at } else { admitted };
            admitted = admitted.saturating_add(1);
            if index < offset {
                true
            } else if rows.len() < size {
                rows.push(row);
                true
            } else {
                more = true;
                false
            }
        });
        (rows, more)
    }

    /// Visit each row of the level with its place among them, from the resource holding the
    /// `skip`th on: the resources wholly before it are counted by their type, once one of the
    /// type was published, and not published. `false` when `visit` stopped it.
    fn walk(
        &self,
        host: &crate::host::Host,
        skip: usize,
        visit: &mut dyn FnMut(usize, PlacedAction) -> bool,
    ) -> bool {
        let store = host.store();
        // How many rows a resource publishes, by its package, its type and whether it is a
        // document.
        let mut weights: BTreeMap<(String, String, bool), usize> = BTreeMap::new();
        let mut row = 0usize;
        let past = |row: usize, weight: usize| row.saturating_add(weight) <= skip;
        for run in &self.runs {
            match run {
                Run::Given(resource) => {
                    if past(row, resource.actions.len()) {
                        row = row.saturating_add(resource.actions.len());
                        continue;
                    }
                    let holds = !resource.children.is_empty();
                    if !self.visit(resource, holds, &mut row, visit) {
                        return false;
                    }
                }
                Run::Document { doc, types } => {
                    let reader = Reader { store, types };
                    let Some(name) = store
                        .type_of_id(doc)
                        .and_then(|ty| Some(store.kinds().get(&ty)?.name.clone()))
                    else {
                        continue;
                    };
                    let key = (types.meta().app.clone(), name, true);
                    if let Some(&weight) = weights.get(&key)
                        && past(row, weight)
                    {
                        row = row.saturating_add(weight);
                        continue;
                    }
                    let Some((at, resource)) = reader.document_alone(doc) else {
                        continue;
                    };
                    weights.insert(key, resource.actions.len());
                    if !self.visit(&resource, reader.holds_any(&at), &mut row, visit) {
                        return false;
                    }
                }
                Run::Items {
                    holder,
                    listing,
                    items,
                    types,
                } => {
                    let reader = Reader { store, types };
                    let len = listing.len();
                    let mut index = 0usize;
                    // A collection of one type whose length is known: every item publishes as
                    // many rows as the first does, so the page starts at the item holding the
                    // `skip`th row, found by counting.
                    if let [_] = items.as_slice()
                        && listing.len_is_exact()
                        && let Some(first) = listing.first()
                        && let Some(first) = item_of(store, holder, &first, items)
                        && let Some(published) = reader.item_alone(&first)
                        && let Some(weight) = std::num::NonZeroUsize::new(published.actions.len())
                    {
                        index = skip
                            .saturating_sub(row)
                            .checked_div(weight.get())
                            .unwrap_or(0)
                            .min(len);
                        row = row.saturating_add(index.saturating_mul(weight.get()));
                    }
                    while index < len {
                        let Some(id) = listing.get(index) else {
                            break;
                        };
                        index = index.saturating_add(1);
                        let Some(item) = item_of(store, holder, &id, items) else {
                            continue;
                        };
                        let Some(name) = store.kinds().get(&item.ty).map(|kind| kind.name.clone())
                        else {
                            continue;
                        };
                        let key = (types.meta().app.clone(), name, false);
                        if let Some(&weight) = weights.get(&key)
                            && past(row, weight)
                        {
                            row = row.saturating_add(weight);
                            continue;
                        }
                        let Some(resource) = reader.item_alone(&item) else {
                            continue;
                        };
                        weights.insert(key, resource.actions.len());
                        if !self.visit(&resource, reader.holds_any(&item), &mut row, visit) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// Visit the rows of `resource`, one of the level, from `row` on: `holds` says whether it
    /// has resources under it, which it is published without.
    fn visit(
        &self,
        resource: &ResourceSpec,
        holds: bool,
        row: &mut usize,
        visit: &mut dyn FnMut(usize, PlacedAction) -> bool,
    ) -> bool {
        survey_placed_level(
            std::slice::from_ref(resource),
            &self.parent,
            &mut |mut placed| {
                placed.has_children = holds;
                let at = *row;
                *row = at.saturating_add(1);
                visit(at, placed)
            },
        )
    }
}

/// A field of a published resource: the resource's id in the store (`doc` or `doc/item`), and
/// the field the change named.
type Field = (String, String);

/// What the updates since a revision changed, as a reader of an action's availability needs it.
pub enum Moved {
    /// These fields changed, by resource id: a resource's own, and every item's under it.
    These(BTreeMap<String, BTreeSet<String>>),
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
            Self::These(changed) if changed.is_empty() => return false,
            Self::These(changed) => changed,
        };
        if changed.get(resource).is_some_and(|fields| {
            fields.contains(field) || fields.contains(WHOLE) || field == WHOLE
        }) {
            return true;
        }
        // The ids that start with the resource's stand together in the order, its items'
        // among them.
        if changed
            .range::<str, _>((
                std::ops::Bound::Excluded(resource.as_str()),
                std::ops::Bound::Unbounded,
            ))
            .map(|(inner, _)| inner)
            .take_while(|inner| inner.starts_with(resource.as_str()))
            .any(|inner| inner.as_bytes().get(resource.len()) == Some(&b'/'))
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

/// What the updates since a revision moved of the documents' text, as a reader that keeps one
/// entry per resource (the search index) reads it: the documents a change was written in, each
/// read again whole, whatever else the workspace holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextMoves {
    /// What moved, by resource id (`doc` or `doc/item`).
    These {
        /// The resources whose text may have moved: every resource the documents in `whole`
        /// hold now.
        texts: BTreeSet<String>,
        /// The documents that are gone, with everything they held.
        gone: BTreeSet<String>,
        /// The documents read again whole: `texts` names every resource each holds now, so an
        /// entry under one of them that `texts` does not name is of an item that is gone.
        whole: BTreeSet<String>,
    },
    /// Every resource: an update since read them all again.
    Everything,
}

/// What a change in the documents `moved` names may have moved of their text: the resources
/// whose text a change wrote, and those it made or removed, read from the fields the store's
/// watch names rather than the documents whole, so a change to one task of ten thousand touches
/// one entry. A document whose change names no fields is read whole, as is one whose own
/// resource was replaced or removed: what it held came or went with it.
fn text_moves(store: &ResourceStore, moved: &crate::resources::Moved) -> TextMoves {
    let mut texts = BTreeSet::new();
    let mut gone = BTreeSet::new();
    let mut whole = BTreeSet::new();
    for doc in &moved.docs {
        let named: Option<&BTreeSet<(String, String)>> = moved
            .fields
            .get(doc)
            .filter(|fields| !fields.is_empty())
            .filter(|fields| {
                !fields
                    .iter()
                    .any(|(resource, field)| resource == doc && field == ANY_FIELD)
            });
        let Some(fields) = named else {
            let Some(ty) = store.type_of_id(doc) else {
                gone.insert(doc.clone());
                continue;
            };
            texts.insert(doc.clone());
            whole.insert(doc.clone());
            every_item(store, &ResourceRef::root(doc.as_str(), ty), &mut texts);
            continue;
        };
        let kinds = store.kinds();
        for (resource, field) in fields {
            let Some(ty) = store.type_of_id(resource) else {
                gone.insert(resource.clone());
                continue;
            };
            // What a search entry holds of a resource is its text: a change to any other field
            // moves no entry.
            let text = field == ANY_FIELD
                || kinds.get(&ty).is_some_and(|entry| {
                    entry.fields.get(field).is_some_and(|field| {
                        matches!(field.kind, FieldKind::Text | FieldKind::RichText)
                    })
                });
            if text {
                texts.insert(resource.clone());
            }
        }
    }
    TextMoves::These { texts, gone, whole }
}

/// The id of every item `at` holds, its items' items included, into `out`.
fn every_item(store: &ResourceStore, at: &ResourceRef, out: &mut BTreeSet<String>) {
    for (field, items) in collections_of(store, None, &at.ty) {
        let spec = FieldSpec {
            id: field,
            kind: FieldKind::Collection,
        };
        let Ok(listing) = store.listing(at, &spec) else {
            continue;
        };
        for id in listing.iter() {
            let Some(item) = item_of(store, at, &id, &items) else {
                continue;
            };
            if out.insert(item.id_string()) {
                every_item(store, &item, out);
            }
        }
    }
}

/// What a workspace publishes of its resources, kept up to date by reading again only the
/// documents a change was written in.
///
/// The store's watch says which documents moved; each is read again from the store and compared
/// with the tree published of it, so a reader holding the earlier tree learns whether only labels
/// moved ([`Self::labels_since`]). A change elsewhere, or one the store cannot name (the
/// workspace moved on from another device), reads every published document again.
pub struct Publications {
    watch: Watch,
    /// Each the very tree it was while the document publishes the same: a reader holding it
    /// knows by its pointer that nothing in it changed.
    trees: BTreeMap<String, Rc<ResourceSpec>>,
    /// Moves whenever a tree is added, replaced or removed.
    revision: u64,
    /// The last revision at which a tree was added, removed or changed in more than its labels.
    structural: u64,
    /// The labels each recent revision that changed only labels moved, oldest first: what a
    /// reader holding an earlier revision patches instead of reading every tree again.
    relabels: VecDeque<(u64, Vec<(String, String)>)>,
    /// Per app, the action types its documents offer.
    types: BTreeMap<String, Arc<ActionTypes>>,
    /// The words the trees above were published in ([`Words::revision`]): when they read in
    /// another language, every document is published again in it.
    words: u64,
    /// The packages loaded when the trees were published: a document whose type none of them
    /// declares any more, or one a package that loaded since declares, is found by publishing
    /// every document again.
    packages: Vec<tilolang_runtime::PackageId>,
    /// Whether nothing is published yet: the first update publishes every document.
    fresh: bool,
    /// Whether documents are published whole up front, without a window bounding them: what a
    /// caller that reads the whole workspace (the session's service, the directory) is served.
    /// A window's surface turns it off ([`Self::window`]) and publishes what it reaches instead
    /// ([`Self::reached`]).
    whole: bool,
    /// How many documents the updates have read, one per document per turn.
    visited: u64,
    /// How many times the updates read every document in full.
    sweeps: u64,
    /// How many updates saw a change in the workspace, whether or not it changed what a document
    /// is published as: an action's availability follows what its app's requirements read, which
    /// a change may move without moving a label.
    moved: u64,
    /// The fields each recent update saw change, by [`Self::moved`], oldest first, and `None`
    /// for an update that read everything again: what a reader holding an earlier count reads
    /// the requirements of again.
    motions: VecDeque<(u64, Option<Vec<Field>>)>,
    /// What each recent update moved of the documents' text, by [`Self::moved`], oldest first:
    /// what a reader that keeps one entry per resource (the search index) replaces, and what it
    /// drops, instead of reading every document again.
    text_moves: VecDeque<(u64, TextMoves)>,
    /// The documents published as what a window reaches of them, each with that reach: every
    /// other document in [`Self::trees`] is whole.
    reached: BTreeMap<String, Reached>,
    /// What the window showed when the reached trees were read: a frame that shows the same,
    /// with none of those documents moved, reads nothing.
    shown: Option<Vec<String>>,
}

/// What a window reaches of a document, by the resource holding it: the resources under it the
/// window shows, each by its id. A read without one reaches every item of every document.
type Reached = BTreeMap<String, BTreeSet<String>>;

/// What `shown` (published ids of `app`) reaches, by the document it is in: each document with
/// the resources from it down to each one `shown` names, found by the collection holding each.
fn reached_by_doc(store: &ResourceStore, app: &str, shown: &[String]) -> BTreeMap<String, Reached> {
    let prefix = resource_id(app, "");
    let mut out: BTreeMap<String, Reached> = BTreeMap::new();
    for id in shown {
        let Some(path) = id.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let path = tilolang_runtime::value::flat_id(path);
        let Some((doc, _)) = tilolang_runtime::value::split_id(&path) else {
            continue;
        };
        let reached = out.entry(doc.to_owned()).or_default();
        // Every step from what is shown up to the document: the route the window reaches it by.
        let mut at = path.clone();
        while at.contains('/') {
            let Some(here) = store
                .type_of_id(&at)
                .and_then(|ty| ResourceRef::parse_id(&at, ty))
            else {
                break;
            };
            let Ok(Some((holder, _))) = store.container(&here) else {
                break;
            };
            let holder = holder.id_string();
            // A step known already: the route above it is too.
            if !reached.entry(holder.clone()).or_default().insert(at) {
                break;
            }
            at = holder;
        }
    }
    out
}

/// How `new`, a document's tree read again, differs from `old`, the one published: whether it
/// holds the same resources in the same places; the labels that moved go to `labels`.
fn same_shape(old: &ResourceSpec, new: &ResourceSpec, labels: &mut Vec<(String, String)>) -> bool {
    if old.id != new.id || old.kind != new.kind || old.children.len() != new.children.len() {
        return false;
    }
    if old.label != new.label {
        labels.push((new.id.as_str().to_owned(), new.label.clone()));
    }
    old.children
        .iter()
        .zip(&new.children)
        .all(|(old, new)| same_shape(old, new, labels))
}

/// Keep `entry` as the newest of `journal`, which holds the last `kept`.
fn keep<T>(journal: &mut VecDeque<T>, entry: T, kept: usize) {
    if journal.len() >= kept {
        journal.pop_front();
    }
    journal.push_back(entry);
}

impl Publications {
    /// Nothing published yet; the first [`Self::update`] publishes every document whole: what the
    /// session's own service and the directory read.
    #[must_use]
    pub fn new(host: &crate::host::Host) -> Self {
        Self {
            watch: host.store().watch(),
            trees: BTreeMap::new(),
            revision: 0,
            structural: 0,
            relabels: VecDeque::new(),
            types: BTreeMap::new(),
            words: host.runtime().words().revision(),
            packages: host.package_ids(),
            fresh: true,
            whole: true,
            visited: 0,
            sweeps: 0,
            moved: 0,
            motions: VecDeque::new(),
            text_moves: VecDeque::new(),
            reached: BTreeMap::new(),
            shown: None,
        }
    }

    /// Publish what a window reaches, not the whole workspace: a window's surface holds the
    /// rows on screen ([`Self::reached`]) and the resource an invocation names
    /// ([`Self::addressed`]), and nothing else until it reaches it. A workspace of ten thousand
    /// costs an open what it shows.
    ///
    /// A caller that reads the whole workspace (the session's own service, the directory) keeps
    /// publishing every document ([`Self::everything`]).
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

    /// The document `doc` read from the store now, with what `reached` reaches of it, or every
    /// item when `None`: `None` when the store holds no such document or no loaded package
    /// declares its type.
    fn read(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        doc: &str,
        reached: Option<&Reached>,
    ) -> Option<ResourceSpec> {
        let types = self.types_of_doc(host, planner, doc)?;
        self.visited = self.visited.saturating_add(1);
        Reader {
            store: host.store(),
            types: &types,
        }
        .document(doc, reached)
    }

    /// The action types the document `doc` is published with, its package's: `None` when the
    /// store holds no such document or no loaded package declares its type.
    fn types_of_doc(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        doc: &str,
    ) -> Option<Arc<ActionTypes>> {
        let ty = host.store().type_of_id(doc)?;
        let app = host.store().kinds().get(&ty)?.app.clone();
        let types = self.types_of(host, &app, planner)?;
        types
            .meta()
            .types
            .iter()
            .any(|declared| declared.id == ty)
            .then_some(types)
    }

    /// Every document of the workspace: the resources of the loaded packages that no collection
    /// holds.
    fn documents(host: &crate::host::Host) -> Vec<String> {
        let mut seen = BTreeSet::new();
        host.records()
            .into_iter()
            .map(|(_, _, doc)| doc)
            .filter(|doc| !doc.contains('/') && seen.insert(doc.clone()))
            .collect()
    }

    /// Every document of the workspace, published whole: what a caller that asks about the whole
    /// workspace (the directory) reads. Nothing of it is what a window shows: a window's surface
    /// holds what it reaches ([`Self::reached`]), and the whole workspace is never published to
    /// it.
    pub fn everything(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
    ) -> Vec<Rc<ResourceSpec>> {
        let pending = self.watch.peek();
        Self::documents(host)
            .into_iter()
            .filter_map(|doc| {
                // A whole tree no change reached since it was read is the document as it is.
                if self.whole
                    && !self.reached.contains_key(&doc)
                    && !pending.names(&doc)
                    && let Some(tree) = self.trees.get(&doc)
                {
                    return Some(Rc::clone(tree));
                }
                self.read(host, planner, &doc, None).map(Rc::new)
            })
            .collect()
    }

    /// What the window's surface publishes: the documents `shown` reaches, each with the
    /// resources under it `shown` names, read from the store as it is now. A document the window
    /// shows nothing of is not published: a workspace of ten thousand publishes the rows on
    /// screen.
    ///
    /// The answer stands while the window shows what it showed and none of the documents it
    /// reaches moved since they were read: a frame that changed neither reads nothing.
    pub fn reached(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        app: &str,
        shown: &[String],
    ) -> Vec<Rc<ResourceSpec>> {
        let pending = self.watch.peek();
        let stands = self.shown.as_deref() == Some(shown)
            && self
                .reached
                .keys()
                .all(|doc| self.trees.contains_key(doc) && !pending.names(doc));
        if !stands {
            let reach = reached_by_doc(host.store(), app, shown);
            // What the window no longer reaches is no longer published: a window scrolled away
            // from a document holds no part of it.
            let dropped: Vec<String> = self
                .reached
                .keys()
                .filter(|doc| !reach.contains_key(*doc))
                .cloned()
                .collect();
            for doc in dropped {
                self.unpublish(&doc);
            }
            for (doc, this) in reach {
                let fresh = self.reached.get(&doc) == Some(&this)
                    && self.trees.contains_key(&doc)
                    && !pending.names(&doc);
                if fresh {
                    continue;
                }
                match self.read(host, planner, &doc, Some(&this)) {
                    Some(tree) => {
                        self.trees.insert(doc.clone(), Rc::new(tree));
                        self.reached.insert(doc, this);
                    }
                    None => self.unpublish(&doc),
                }
            }
            // The revision counts what a reader of the whole workspace saw: a reached tree is
            // read for the frame that reached it, which the surface holds by the ids it shows.
            self.shown = Some(shown.to_vec());
        }
        self.reached
            .keys()
            .filter_map(|doc| self.trees.get(doc).cloned())
            .collect()
    }

    /// The document of the resource `address` names, published down to it: what an invocation is
    /// resolved against when the window shows nothing of it. A document of any package the host
    /// runs is reached so, the ones its own app uses too: what a window shows bounds what it
    /// publishes, never what a caller reaches, which the API decides by the caller's grants.
    /// `None` when the address is no resource of a package the host runs.
    pub fn addressed(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        address: &str,
    ) -> Option<Rc<ResourceSpec>> {
        let app = app_of(address)?;
        host.package(app)?;
        let reached = reached_by_doc(host.store(), app, &[address.to_owned()]);
        let (doc, reached) = reached.into_iter().next()?;
        self.read(host, planner, &doc, Some(&reached)).map(Rc::new)
    }

    /// One level of what [`Self::everything`] publishes: the documents at the top when `parent`
    /// is `None`, else the resources directly under the published resource `parent`, in the
    /// order the whole publication holds them. Nothing of it is published yet: a page of it
    /// publishes the resources it lists ([`Level::page`]), whatever else the workspace holds. A
    /// `parent` the whole publication holds no resource as has nothing under it.
    pub fn level(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        parent: Option<&str>,
    ) -> Level {
        let Some(parent) = parent else {
            let runs = Self::documents(host)
                .into_iter()
                .filter_map(|doc| {
                    let types = self.types_of_doc(host, planner, &doc)?;
                    Some(Run::Document { doc, types })
                })
                .collect();
            return Level {
                parent: String::new(),
                runs,
            };
        };
        let runs = self
            .published_at(host, planner, parent)
            .map(|(at, types)| {
                Reader {
                    store: host.store(),
                    types: &types,
                }
                .collections(&at)
                .into_iter()
                .map(|(listing, items)| Run::Items {
                    holder: at.clone(),
                    listing,
                    items,
                    types: Arc::clone(&types),
                })
                .collect()
            })
            .unwrap_or_default();
        Level {
            parent: parent.to_owned(),
            runs,
        }
    }

    /// The resource the whole publication publishes as `published` (`tl:<app>:<doc>` or
    /// `tl:<app>:<doc>/<item>`), as its tree holds it, with the action types of its document:
    /// found from what the store records holds it, then followed down from its document as
    /// [`Reader::items`] reaches it, collection by collection. `None` when no tree holds it.
    fn published_at(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        published: &str,
    ) -> Option<(ResourceRef, Arc<ActionTypes>)> {
        let app = app_of(published)?;
        let path = published.strip_prefix(resource_id(app, "").as_str())?;
        let (doc, item) = tilolang_runtime::value::split_id(path)?;
        let types = self.types_of_doc(host, planner, doc)?;
        let store = host.store();
        let mut at = ResourceRef::root(doc, store.type_of_id(doc)?);
        if item.is_some() {
            // The steps from the document down to it: each item, and the collection holding it.
            let mut here = ResourceRef::parse_id(path, store.type_of_id(path)?)?;
            let mut steps = Vec::new();
            while let Some(id) = here.item.clone() {
                let (holder, field) = store.container(&here).ok()??;
                steps.push((id, field));
                here = holder;
            }
            if here.doc != doc {
                return None;
            }
            for (id, field) in steps.into_iter().rev() {
                let (field, items) = collections_of(store, Some(types.meta()), &at.ty)
                    .into_iter()
                    .find(|(of, _)| *of == field)?;
                let spec = FieldSpec {
                    id: field,
                    kind: FieldKind::Collection,
                };
                if store.listing(&at, &spec).ok()?.holds(&id) == Some(false) {
                    return None;
                }
                let next = item_of(store, &at, &id, &items)?;
                store.kinds().get(&next.ty)?;
                at = next;
            }
        }
        (resource_id(&types.meta().app, &at.id_string()) == published).then_some((at, types))
    }

    /// Publish nothing of `doc` any more: a document the window scrolled away from holds no part
    /// of it, and one the workspace no longer has is gone.
    fn unpublish(&mut self, doc: &str) {
        self.reached.remove(doc);
        self.trees.remove(doc);
    }

    /// How many documents the updates so far have read, one per document per turn: what
    /// publishing a change costs, whatever the workspace holds.
    #[must_use]
    pub fn visited(&self) -> u64 {
        self.visited
    }

    /// Whether what is published is behind what [`Self::update`] would read from `host`: the
    /// store moved since the last update took in what moved (a write the window made itself, a
    /// task checked off at its palette or an invocation it ran, as much as one another device
    /// made), the words it is published in read in another language now, or the packages
    /// loaded changed. What it offers of each action is behind with it until the next update.
    #[must_use]
    pub fn behind(&self, host: &crate::host::Host) -> bool {
        self.fresh
            || self.watch.moved()
            || host.runtime().words().revision() != self.words
            || host.package_ids() != self.packages
    }

    /// How many updates have seen a change in the workspace, whether or not it changed what a
    /// document is published as: what an action's availability, and only it, reads besides.
    #[must_use]
    pub fn moved(&self) -> u64 {
        self.moved
    }

    /// The fields that changed since the update counted `since`, each once; everything when an
    /// update since read it all again, and `None` when what changed that long ago is no longer
    /// kept, which reads everything too.
    ///
    /// A change names the fields it wrote, which reads again the requirements that read them;
    /// one that names none reads again those of its whole document and every item in it.
    #[must_use]
    pub fn changed_since(&self, since: u64) -> Option<Moved> {
        if since == self.moved {
            return Some(Moved::These(BTreeMap::new()));
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
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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

    /// What the updates since `since` moved of the documents' text, each once: the entries a
    /// reader that keeps one per resource reads again, and those it drops. `None` when what
    /// moved that long ago is no longer kept, which reads everything too.
    #[must_use]
    pub fn text_since(&self, since: u64) -> Option<TextMoves> {
        let mut texts = BTreeSet::new();
        let mut gone = BTreeSet::new();
        let mut whole = BTreeSet::new();
        if since == self.moved {
            return Some(TextMoves::These { texts, gone, whole });
        }
        if since > self.moved {
            return None;
        }
        let first = self.text_moves.iter().position(|(at, _)| *at > since)?;
        if self.text_moves.get(first)?.0 != since.wrapping_add(1) {
            return None;
        }
        // In the order they happened: a document gone and made again is there, one read and
        // then gone is gone.
        for (_, moved) in self.text_moves.iter().skip(first) {
            match moved {
                TextMoves::Everything => return Some(TextMoves::Everything),
                TextMoves::These {
                    texts: these_texts,
                    gone: these_gone,
                    whole: these_whole,
                } => {
                    for doc in these_gone {
                        texts.remove(doc);
                        whole.remove(doc);
                        gone.insert(doc.clone());
                    }
                    for doc in these_whole {
                        gone.remove(doc);
                        whole.insert(doc.clone());
                    }
                    texts.extend(these_texts.iter().cloned());
                }
            }
        }
        Some(TextMoves::These { texts, gone, whole })
    }

    /// How many times the updates so far have read every document in full: only the first
    /// update, and one after the workspace moved on in a way the store could not name.
    #[must_use]
    pub fn sweeps(&self) -> u64 {
        self.sweeps
    }

    /// The published documents, by document id. A document whose tree did not change since an
    /// earlier call is the very same tree.
    #[must_use]
    pub fn trees(&self) -> &BTreeMap<String, Rc<ResourceSpec>> {
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
        let moved = self.watch.take();
        // The words a package is published in read in another language now (the person chose
        // one, or a catalogue of it was added): every document is read again, so its labels,
        // descriptions and nouns say what they are now called.
        let reworded = {
            let words = host.runtime().words().revision();
            std::mem::replace(&mut self.words, words) != words
        };
        if reworded {
            self.types.clear();
        }
        // A package loaded or replaced: the documents of a type none declares any more go, and
        // those of a type one declares now are published.
        let reloaded = std::mem::replace(&mut self.packages, host.package_ids()) != self.packages;
        let all = std::mem::take(&mut self.fresh) || reworded || reloaded || moved.everything;
        let mut touched = Vec::new();
        let mut relabels = Some(Vec::new());
        if all {
            relabels = None;
            if self.whole {
                self.publish_everything(host, planner, &mut touched);
            } else {
                // Nothing is published up front: what a window reaches is read again when it
                // reaches it ([`Self::reached`]), and a caller that reads the whole workspace
                // asks for it ([`Self::everything`]). A workspace of ten thousand costs an open
                // what it shows.
                self.forget(&mut touched);
            }
        } else {
            // Only the documents a change was written in are read, whether the workspace holds
            // one or ten thousand.
            for doc in &moved.docs {
                self.republish(host, planner, doc, &mut relabels, &mut touched);
            }
        }
        // A change may move what a requirement of an action reads without moving any tree of
        // its own (a task checked, and `Clear done` can run now): a surface holding the actions
        // reads the requirements of the documents that changed again.
        if all || !moved.is_empty() {
            self.moved = self.moved.saturating_add(1);
            // The fields each change wrote, so a requirement reading a field of a document that
            // changed elsewhere stands; the whole document where a change named none.
            let fields = (!all).then(|| {
                moved
                    .docs
                    .iter()
                    .flat_map(|doc| match moved.fields.get(doc) {
                        Some(fields) if !fields.is_empty() => fields.iter().cloned().collect(),
                        _ => vec![(doc.clone(), tilolang_runtime::store::ANY_FIELD.to_owned())],
                    })
                    .collect()
            });
            keep(&mut self.motions, (self.moved, fields), MOTIONS_KEPT);
            let text = if all {
                TextMoves::Everything
            } else {
                text_moves(host.store(), &moved)
            };
            keep(&mut self.text_moves, (self.moved, text), MOTIONS_KEPT);
        }
        touched.sort();
        touched.dedup();
        if !touched.is_empty() {
            self.revision = self.revision.wrapping_add(1);
            match relabels {
                Some(labels) => keep(&mut self.relabels, (self.revision, labels), RELABELS_KEPT),
                None => self.structural = self.revision,
            }
        }
        touched
    }

    /// Read the document `doc` again after a change was written in it, as much of it as is
    /// published: whole, or what the window reaches of it. Nothing of a document nothing is
    /// published of. The labels a change moved go to `relabels`, which is `None` once a tree
    /// changed in more than its labels.
    fn republish(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        doc: &str,
        relabels: &mut Option<Vec<(String, String)>>,
        touched: &mut Vec<String>,
    ) {
        let reach = self.reached.get(doc).cloned();
        if !self.whole && reach.is_none() {
            return;
        }
        let Some(tree) = self.read(host, planner, doc, reach.as_ref()) else {
            // The resource is gone: so is everything published of it.
            if self.trees.contains_key(doc) {
                self.unpublish(doc);
                *relabels = None;
                touched.push(doc.to_owned());
            }
            return;
        };
        match self.trees.get(doc) {
            Some(old) => {
                let mut labels = Vec::new();
                if same_shape(old, &tree, &mut labels) {
                    if labels.is_empty() {
                        // It publishes what it did: the very tree it was stays.
                        return;
                    }
                    if let Some(gathered) = relabels.as_mut() {
                        gathered.extend(labels);
                    }
                } else {
                    *relabels = None;
                }
            }
            None => *relabels = None,
        }
        self.trees.insert(doc.to_owned(), Rc::new(tree));
        touched.push(doc.to_owned());
    }

    /// Publish nothing, forgetting what was published: what a turn that changed what every
    /// document is published as costs a window — every document's tree is read again when
    /// something reaches it.
    fn forget(&mut self, touched: &mut Vec<String>) {
        self.shown = None;
        self.reached.clear();
        touched.extend(std::mem::take(&mut self.trees).into_keys());
    }

    /// Publish every document in full: what a caller that reads the whole workspace asks for (the
    /// session's own service, the directory).
    fn publish_everything(
        &mut self,
        host: &crate::host::Host,
        planner: &Arc<dyn Planner>,
        touched: &mut Vec<String>,
    ) {
        let docs = Self::documents(host);
        let now: BTreeSet<&str> = docs.iter().map(String::as_str).collect();
        let gone: Vec<String> = self
            .trees
            .keys()
            .filter(|doc| !now.contains(doc.as_str()))
            .cloned()
            .collect();
        for doc in gone {
            self.unpublish(&doc);
            touched.push(doc);
        }
        self.reached.clear();
        self.shown = None;
        self.sweeps = self.sweeps.saturating_add(1);
        for doc in docs {
            let Some(tree) = self.read(host, planner, &doc, None) else {
                if self.trees.remove(&doc).is_some() {
                    touched.push(doc);
                }
                continue;
            };
            let unchanged = self.trees.get(&doc).is_some_and(|old| {
                let mut labels = Vec::new();
                same_shape(old, &tree, &mut labels) && labels.is_empty()
            });
            if unchanged {
                continue;
            }
            self.trees.insert(doc.clone(), Rc::new(tree));
            touched.push(doc);
        }
    }
}

/// How many label-only revisions [`Publications::labels_since`] reaches back over.
const RELABELS_KEPT: usize = 256;

/// How many updates [`Publications::changed_since`] reaches back over.
const MOTIONS_KEPT: usize = 256;

/// The package's own resource, carrying the actions that take no resource and those whose subject
/// is one of the app's roots.
///
/// An app keeps one resource of each of its root types in a workspace, so an action on a root is
/// unambiguous at the package's own resource (`tl:<app>#<action>`): the caller reaches it there
/// whether or not a window of the app has opened the root, as an agent, a device and `tilo do` do.
/// The run acts on the root's document, which the first change to it makes.
#[must_use]
pub fn publish_package(meta: &Meta, planner: &Arc<dyn Planner>, words: &dyn Words) -> ResourceSpec {
    let kind = meta.package_kind();
    let mut resource = ResourceSpec::new(
        package_resource_id(&meta.app),
        kind.clone(),
        app_name(words, meta),
    );
    // An app may declare one action name on more than one of its types, and each resource then
    // tells them apart; the package's own resource cannot, so it offers a name once. A name an
    // action that takes no resource has is that action's: it has no other address.
    let without_resource: BTreeSet<&str> = meta
        .offered_actions()
        .filter(|action| meta.target_of(&action.params).is_none())
        .map(crate::meta::published_name)
        .collect();
    let mut offered = BTreeSet::new();
    for action in meta.offered_actions() {
        // An action whose first parameter takes one record is published on that type
        // ([`ActionTypes::new`]); one whose parameter takes a union of types is offered here,
        // taking the resource it acts on as a reference argument, and one whose subject is a
        // root is offered here too, acting on the root.
        let target_ty = meta.target_of(&action.params);
        let on_a_root = target_ty.is_some();
        let target = match target_ty {
            None => Target::none(),
            Some(target) => {
                let Some(doc) = root_document(meta, &target.id) else {
                    continue;
                };
                Target::new(doc)
            }
        };
        let name = crate::meta::published_name(action);
        if (on_a_root && without_resource.contains(name)) || !offered.insert(name) {
            continue;
        }
        let mut spec = action_type(meta, &kind, action, planner, words).bind(target);
        // The root's own resource offers the action where a person works with it; the package's
        // row is the one an agent, a script or a device names, and a second row of the same name
        // in a palette helps no one.
        if on_a_root {
            spec = spec.for_audience(tilo_actions::Audience::Agents);
        }
        resource = resource.with_action(spec);
    }
    resource
}

/// The document the app's root of `ty` stands at (`root-<type id>`), when `ty` is one of its roots
/// people share; `None` for any other type, and for a root each device keeps its own, whose
/// document names the device it stands on.
fn root_document(meta: &Meta, ty: &str) -> Option<String> {
    meta.shared_roots()
        .any(|root| root == ty)
        .then(|| format!("root-{ty}"))
}

/// The host's own resource: the [`HostEffect`](crate::effects::HostEffect) vocabulary an agent
/// uses (run a block, read the catalogue), which the window answers with `Host::answer`. It is not
/// offered in the palette.
#[must_use]
pub fn host_resource() -> ResourceSpec {
    let mut host = ResourceSpec::new(HOST_RESOURCE, crate::surface::HOST_KIND, "tilolang");
    for spec in <crate::effects::HostEffect as tilo_resource::Effect>::specs() {
        host = host.with_action(spec);
    }
    host
}

/// The plain edit a bound control makes, written to `store` as `invoker`'s change: set `value`
/// into the field `field` of the resource `id` (`doc` or `doc/item`) of the type `type_name`. It
/// runs no module code, so it needs no planner.
///
/// `field` is the field's **name**, as the app declares it (`title`, `body`), which the edit
/// verbs publish as their `field` choices; the internal id it is stored under is mapped here. An
/// id is accepted too: a bound control hands back the id of the field it shows.
///
/// # Errors
/// The type has no such field, the field cannot take the value, or the store refused the write.
pub fn edit_action(
    store: &ResourceStore,
    meta: &Meta,
    type_name: &str,
    id: &str,
    field: &str,
    value: &Json,
    invoker: Option<&Actor>,
    readers: &dyn Fn(&ResourceRef) -> Option<Vec<tilo_actions::MemberId>>,
) -> Result<DynAction, String> {
    let declared = meta
        .type_named(type_name)
        .ok_or_else(|| format!("the package declares no type `{type_name}`"))?;
    let no_field = || {
        format!(
            "the type `{type_name}` has no field `{field}`; its fields are: {}",
            field_names(declared)
        )
    };
    // The field as the type declares it: by its name, the spelling the edit verbs offer, or
    // by its id, the spelling a bound control hands back.
    let field_id = declared
        .fields
        .iter()
        .find(|known| known.name == field)
        .or_else(|| declared.fields.iter().find(|known| known.id == field))
        .ok_or_else(no_field)?
        .id
        .clone();
    let kind = store
        .kinds()
        .get(&declared.id)
        .and_then(|entry| entry.fields.get(&field_id))
        .map(|entry| entry.kind)
        .ok_or_else(no_field)?;
    let mutation = match (kind, value) {
        (FieldKind::Text, Json::String(text)) => Mutation::SetText {
            field: field_id.clone(),
            text: text.clone(),
        },
        (FieldKind::Register, value) => Mutation::SetRegister {
            field: field_id.clone(),
            value: scalar_of(value),
        },
        // Bytes as the stored form names them: the data itself when small, else the content
        // store's address of it, which the invoking side kept.
        (FieldKind::Bytes, value) => Mutation::SetBytes {
            field: field_id.clone(),
            data: BytesData::from_json(value)
                .map_err(|why| format!("the field `{field}` takes bytes: {why}"))?,
        },
        // A rich text comes as its words, a paragraph a line, or as the runtime's encoding of
        // it where it carries more than words.
        (FieldKind::RichText, value) => Mutation::SetRich {
            field: field_id.clone(),
            value: match value {
                Json::String(text) => tilolang_runtime::rich::RichValue::from_text(text),
                encoded => match tilolang_runtime::value::from_json(encoded)
                    .map_err(|why| format!("the field `{field}` takes a rich text: {why}"))?
                {
                    tilolang_runtime::value::Value::Rich(rich) => rich,
                    _ => return Err(format!("the field `{field}` takes a rich text")),
                },
            },
        },
        _ => return Err(format!("the field `{field}` cannot be set to that value")),
    };
    let at = ResourceRef::parse_id(id, declared.id.clone())
        .ok_or_else(|| format!("`{id}` is not a resource id"))?;
    let action_id = if value.is_boolean() {
        crate::action::EDIT_FLAG_ACTION
    } else {
        crate::action::EDIT_TEXT_ACTION
    };
    let args = serde_json::json!({"field": field_id, "value": value});
    commit_edit(
        store,
        meta,
        EditChange {
            kind: &meta.kind_of(type_name),
            at: &at,
            mutation: &mutation,
            action_id,
            args,
            invoker,
        },
        readers,
    )
}

/// The write a direct edit makes: the resource `at` of type `kind`, the `mutation` that sets it,
/// and the change the action `action_id` records it as, with the `args` that name what it
/// changed and `invoker` who made it.
struct EditChange<'a> {
    kind: &'a str,
    at: &'a ResourceRef,
    mutation: &'a Mutation,
    action_id: &'a str,
    args: Json,
    invoker: Option<&'a Actor>,
}

/// Write `change` to the store as its invoker's: the write every direct edit makes, whether it
/// sets a field or one paragraph of its rich text.
fn commit_edit(
    store: &ResourceStore,
    meta: &Meta,
    change: EditChange<'_>,
    readers: &dyn Fn(&ResourceRef) -> Option<Vec<tilo_actions::MemberId>>,
) -> Result<DynAction, String> {
    let EditChange {
        kind,
        at,
        mutation,
        action_id,
        args,
        invoker,
    } = change;
    let mut store = store.clone();
    let txn = store.begin().map_err(|error| error.to_string())?;
    if let Err(error) = store.apply(txn, at, mutation) {
        store.rollback(txn);
        return Err(error.to_string());
    }
    // Asked with the field as the edit wrote it: an edit that makes a draft sent makes a
    // change every member reads.
    let readers = readers(at);
    store
        .commit(
            txn,
            &ChangeNote {
                action: action_id.to_owned(),
                args: args.clone(),
                invoker: invoker.cloned(),
                module: meta.module_hash.clone(),
                readers: readers.clone(),
                ..ChangeNote::default()
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(DynAction {
        app: meta.app.clone(),
        kind: kind.to_owned(),
        action_id: action_id.to_owned(),
        args,
        authorship: crate::action::Authorship {
            invoker: invoker.cloned(),
            module: meta.module_hash.clone(),
            readers,
            ..crate::action::Authorship::default()
        },
    })
}

/// The value a register holds of `value`, as the API hands it: a flag, a number or a text as
/// itself, anything else as the runtime's encoding of it.
fn scalar_of(value: &Json) -> Scalar {
    match value {
        Json::Bool(flag) => Scalar::Bool(*flag),
        Json::String(text) => Scalar::Text(text.clone()),
        Json::Number(number) => number.as_i64().map_or_else(
            || Scalar::Float(number.as_f64().unwrap_or_default()),
            Scalar::Int,
        ),
        other => Scalar::Json(other.clone()),
    }
}

/// The direct edit `request` asks for ([`edit_action`]), written to `store` as `invoker`'s: what
/// the host plans when the API builds an `edit-text` or `edit-flag` invocation, so an edit is a
/// trial run like any action's plan unless the invocation is being carried out. `None` when
/// `request` is no direct edit of a type of `meta`.
pub fn plan_edit(
    store: &ResourceStore,
    meta: &Meta,
    request: &PlanRequest<'_>,
    invoker: &Actor,
    readers: &dyn Fn(&ResourceRef) -> Option<Vec<tilo_actions::MemberId>>,
) -> Option<Result<DynAction, String>> {
    if let Some(edit) = BLOCK_EDITS
        .into_iter()
        .find(|edit| edit.id == request.action_id)
    {
        let declared = meta
            .types
            .iter()
            .find(|declared| meta.kind_of(&declared.name) == request.kind)?;
        let Some(id) = request.resource else {
            return Some(Err(
                "a paragraph edit names the resource it edits".to_owned()
            ));
        };
        return Some(edit_block_action(
            store,
            meta,
            &declared.name,
            id,
            request.args,
            edit,
            Some(invoker),
            readers,
        ));
    }
    let flag = DIRECT_EDITS
        .iter()
        .find(|direct| direct.id == request.action_id)?
        .value
        == ParamType::Boolean;
    let declared = meta
        .types
        .iter()
        .find(|declared| meta.kind_of(&declared.name) == request.kind)?;
    let Some(id) = request.resource else {
        return Some(Err("a direct edit names the resource it edits".to_owned()));
    };
    let field = request
        .args
        .get("field")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let mut value = request.args.get("value").cloned().unwrap_or(Json::Null);
    // A flag may come as its text, as an API that knows no parameter type for it hands it on.
    if flag && let Some(said) = value.as_str() {
        match said.parse::<bool>() {
            Ok(set) => value = Json::Bool(set),
            Err(_) => return Some(Err(format!("`{said}` is not a flag"))),
        }
    }
    Some(edit_action(
        store,
        meta,
        &declared.name,
        id,
        field,
        &value,
        Some(invoker),
        readers,
    ))
}

/// The action a direct edit (`action_id`, one of [`DIRECT_EDITS`]) of a field of `declared`
/// is, where an action of `meta` settles that field (`settles: t.status`): its id, and the
/// arguments it takes the edit's value as. A bound control's edit, another app's program and an
/// agent then set the field the way the app's own controls do, and what the action keeps in step
/// with it follows. `None` for any other action, and for a field no action settles.
#[must_use]
pub fn settled_edit(
    meta: &Meta,
    declared: &TypeMeta,
    action_id: &str,
    args: &serde_json::Map<String, Json>,
) -> Option<(String, serde_json::Map<String, Json>)> {
    let direct = DIRECT_EDITS.iter().find(|direct| direct.id == action_id)?;
    let field = args.get("field").and_then(Json::as_str)?;
    let field = declared
        .fields
        .iter()
        .find(|known| known.name == field)
        .or_else(|| declared.fields.iter().find(|known| known.id == field))?;
    let settler = meta
        .actions
        .iter()
        .find(|action| !action.scenario && action.settles.as_deref() == Some(field.id.as_str()))?;
    let param = settler.params.get(1)?;
    let mut value = args.get("value").cloned().unwrap_or(Json::Null);
    // A flag may come as its text, as an API that knows no parameter type for it hands it on.
    if direct.value == ParamType::Boolean
        && let Some(said) = value.as_str()
        && let Some(set) = tilo_failure::ignored!(
            "a flag given as other words goes to the action as given, which refuses it",
            said.parse::<bool>().ok()
        )
    {
        value = Json::Bool(set);
    }
    Some((
        settler.id.clone(),
        serde_json::Map::from_iter([(param.name.clone(), value)]),
    ))
}

/// The names of `declared`'s fields, comma-separated, as a refusal that names a field the
/// type lacks lists them: what an [`edit_action`] refusal says a caller may choose instead.
fn field_names(declared: &TypeMeta) -> String {
    declared
        .fields
        .iter()
        .map(|known| known.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The paragraph edit `edit` `args` asks for, written to `store` as `invoker`'s: the paragraph of
/// a rich-text field of the resource `id` that `args.block` names
/// (`tl:<app>:<id>#<field>.<place>`) is set to what its Markdown says, preceded by it, or removed
/// ([`crate::action::BLOCK_EDITS`]). The text is read and written as the value it already holds
/// with that one paragraph changed, which the store writes as the splices of that paragraph: a
/// concurrent edit of another paragraph merges, and history keeps one change.
///
/// # Errors
/// The type has no such rich-text field, the address names no paragraph the text holds or names
/// another resource, the text is no document, or the store refused the write.
pub fn edit_block_action(
    store: &ResourceStore,
    meta: &Meta,
    type_name: &str,
    id: &str,
    args: &serde_json::Map<String, Json>,
    edit: BlockEdit,
    invoker: Option<&Actor>,
    readers: &dyn Fn(&ResourceRef) -> Option<Vec<tilo_actions::MemberId>>,
) -> Result<DynAction, String> {
    let declared = meta
        .type_named(type_name)
        .ok_or_else(|| format!("the package declares no type `{type_name}`"))?;
    let address = args
        .get("block")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .trim();
    if address.is_empty() {
        return Err("name the paragraph as `tl:<app>:<id>#<field>.<place>`".to_owned());
    }
    let (field, place) = paragraph_at(meta, id, address)?;
    let no_field = || {
        format!(
            "the type `{type_name}` has no field `{field}`; its fields are: {}",
            field_names(declared)
        )
    };
    let field_id = declared
        .fields
        .iter()
        .find(|known| known.name == field)
        .or_else(|| declared.fields.iter().find(|known| known.id == field))
        .map(|known| known.id.clone())
        .ok_or_else(no_field)?;
    let kind = store
        .kinds()
        .get(&declared.id)
        .and_then(|entry| entry.fields.get(&field_id))
        .map(|entry| entry.kind)
        .ok_or_else(no_field)?;
    if kind != FieldKind::RichText {
        return Err(format!(
            "the field `{field}` is no rich text: a paragraph is changed in a `RichText` field"
        ));
    }
    let at = ResourceRef::parse_id(id, declared.id.clone())
        .ok_or_else(|| format!("`{id}` is not a resource id"))?;
    let held = store
        .read(
            &at,
            &FieldSpec {
                id: field_id.clone(),
                kind,
            },
        )
        .map_err(|error| error.to_string())?;
    let FieldValue::Rich(rich) = held else {
        return Err(format!("the field `{field}` is no rich text"));
    };
    let text = args.get("text").and_then(Json::as_str).unwrap_or_default();
    let said = || -> Result<Vec<tilo_doc::snapshot::BlockView>, String> {
        if text.trim().is_empty() {
            return Err(format!(
                "`{}` takes what the paragraph says; `{}` removes one",
                edit.id,
                crate::action::DELETE_BLOCK_ACTION
            ));
        }
        tilolang_runtime::rich::RichValue::from_markdown(text).map(|rich| rich.blocks)
    };
    let change = match edit.kind {
        BlockEditKind::Replace => tilolang_runtime::rich::BlockChange::Replace(said()?),
        BlockEditKind::Insert => tilolang_runtime::rich::BlockChange::Insert(said()?),
        BlockEditKind::Delete => tilolang_runtime::rich::BlockChange::Delete,
    };
    let value = rich.with_block(place, &change)?;
    let mutation = Mutation::SetRich {
        field: field_id,
        value,
    };
    let mut recorded = serde_json::Map::new();
    recorded.insert("block".to_owned(), Json::String(address.to_owned()));
    if edit.text {
        recorded.insert("text".to_owned(), Json::String(text.to_owned()));
    }
    commit_edit(
        store,
        meta,
        EditChange {
            kind: &meta.kind_of(type_name),
            at: &at,
            mutation: &mutation,
            action_id: edit.id,
            args: Json::Object(recorded),
            invoker,
        },
        readers,
    )
}

/// The field and the place a paragraph address names (`tl:<app>:<id>#<field>.<place>`), checked
/// against the resource `id` of the app: an address that names a resource names this one.
fn paragraph_at<'a>(meta: &Meta, id: &str, address: &'a str) -> Result<(&'a str, usize), String> {
    let (named, at) = match address.split_once('#') {
        Some((named, at)) => (Some(named.trim()), at.trim()),
        None => (None, address),
    };
    if let Some(named) = named
        && named != resource_id(&meta.app, id)
        && named != id
    {
        return Err(format!(
            "the address names `{named}`, not `{}`",
            resource_id(&meta.app, id)
        ));
    }
    let Some((field, place)) = at.rsplit_once('.') else {
        return Err(format!(
            "`{address}` names no paragraph: name it as `tl:<app>:<id>#<field>.<place>`"
        ));
    };
    let place: usize = place
        .trim()
        .parse()
        .map_err(|_| format!("`{}` is no paragraph place", place.trim()))?;
    Ok((field.trim(), place))
}

/// The direct edit actions of a resource, whose target is its id: a bound text input and a bound
/// checkbox, and — for a type with rich text — the edits of one paragraph of it. The verbs and
/// their arguments are [`DIRECT_EDITS`]' and [`BLOCK_EDITS`]'.
fn edit_types(meta: &Meta, type_name: &str, planner: &Arc<dyn Planner>) -> Vec<Arc<ActionType>> {
    // The fields `edit-text` and `edit-flag` may set, by their names: a caller reads the list
    // in the parameter's description rather than guessing at an internal id. The parameter is
    // not restricted to them, though: a bound control's `$edit` hands the field's internal id
    // back (and a nested field path still names a field), and `edit_action` takes either,
    // mapping a name or an id to the field it names and refusing the ones the type lacks with
    // the names it offers. Binding either keeps a control's own edits working.
    let names = |flag: bool| -> Vec<String> {
        let merge = if flag { Merge::Flag } else { Merge::Text };
        meta.type_named(type_name)
            .map(|declared| editable_field_names(declared, merge))
            .unwrap_or_default()
    };
    let declared = meta.type_named(type_name).cloned();
    let make = |direct: DirectEdit| {
        let fields = names(direct.value == ParamType::Boolean);
        let (app, kind) = (meta.app.clone(), meta.kind_of(type_name));
        let (declared, type_name) = (declared.clone(), type_name.to_owned());
        let planner = Arc::clone(planner);
        let description = if fields.is_empty() {
            "The field's name".to_owned()
        } else {
            format!("The field's name, one of: {}", fields.join(", "))
        };
        let field = ActionParam::required_owned("field", ParamType::Text, description);
        let spec = ActionSpec::typed_on(
            direct.id,
            direct.label,
            direct.label,
            ExecutionLocus::ClientLocal,
            vec![
                field,
                ActionParam::required("value", direct.value, "The new value"),
            ],
            move |target: &Target, args: &ActionArgs| {
                let json = args.to_json();
                let refused = |message: String| EncodedAction {
                    type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                    payload: message.into_bytes(),
                };
                // A field the type lacks is refused here, before anything is planned, with the
                // names of the fields it has.
                let field = json.get("field").and_then(Json::as_str).unwrap_or_default();
                if let Some(declared) = &declared
                    && !declared
                        .fields
                        .iter()
                        .any(|known| known.name == field || known.id == field)
                {
                    return refused(format!(
                        "the type `{type_name}` has no field `{field}`; its fields are: {}",
                        field_names(declared)
                    ));
                }
                // The edit is the caller's the API admitted: an agent's change is the agent's,
                // in history and wherever the field shows who changed it.
                let caller = planner.caller();
                let request = PlanRequest {
                    app: &app,
                    kind: &kind,
                    action_id: direct.id,
                    resource: target.get::<String>().map(String::as_str),
                    args: &json,
                    caller: caller.as_ref(),
                    from: None,
                };
                planner
                    .plan(&request)
                    .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
                    .unwrap_or_else(refused)
            },
        )
        // The platform runs it for a bound field, and an agent may ask for it; a person
        // changes the field in the control they see, whose own action they press.
        .for_audience(tilo_actions::Audience::Agents);
        Arc::clone(spec.action_type())
    };
    let mut out: Vec<Arc<ActionType>> = DIRECT_EDITS.into_iter().map(make).collect();
    // A type with rich text offers one paragraph of it, by its address, to an agent: block
    // search finds a paragraph and names it so, and the edit is a splice of that paragraph.
    let rich = declared.as_ref().map(rich_field_names).unwrap_or_default();
    if !rich.is_empty() {
        let described = format!(
            "The paragraph's address, `tl:<app>:<id>#<field>.<place>`, as block search names one; \
             the text's fields: {}",
            rich.join(", ")
        );
        for edit in BLOCK_EDITS {
            out.push(Arc::clone(&block_edit_type(
                meta, type_name, planner, edit, &described,
            )));
        }
    }
    out
}

/// The action type of the paragraph edit `edit` of the resources of `type_name` of `meta`, whose
/// `block` parameter is described by `described`: it plans through `planner`, as the direct
/// edits do, and an agent is its audience.
fn block_edit_type(
    meta: &Meta,
    type_name: &str,
    planner: &Arc<dyn Planner>,
    edit: BlockEdit,
    described: &str,
) -> Arc<ActionType> {
    let (app, kind) = (meta.app.clone(), meta.kind_of(type_name));
    let planner = Arc::clone(planner);
    let mut params = vec![ActionParam::required_owned(
        "block",
        ParamType::Text,
        described.to_owned(),
    )];
    if edit.text {
        params.push(ActionParam::required(
            "text",
            ParamType::Text,
            "What the paragraph says, as Markdown",
        ));
    }
    let spec = ActionSpec::typed_on(
        edit.id,
        edit.label,
        edit.label,
        ExecutionLocus::ClientLocal,
        params,
        move |target: &Target, args: &ActionArgs| {
            let json = args.to_json();
            let refused = |message: String| EncodedAction {
                type_id: INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: message.into_bytes(),
            };
            let caller = planner.caller();
            let request = PlanRequest {
                app: &app,
                kind: &kind,
                action_id: edit.id,
                resource: target.get::<String>().map(String::as_str),
                args: &json,
                caller: caller.as_ref(),
                from: None,
            };
            planner
                .plan(&request)
                .and_then(|planned| planned.encode().map_err(|error| error.to_string()))
                .unwrap_or_else(refused)
        },
    )
    .for_audience(tilo_actions::Audience::Agents);
    Arc::clone(spec.action_type())
}
