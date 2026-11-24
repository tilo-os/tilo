//! [`Host`] — the tilolang runtime wired to the platform, without any UI.
//!
//! A host owns one [`Runtime`] over a [`ResourceStore`] and the platform's [`TiloAuthority`]. It
//! compiles and loads packages, keeps their record schemas registered, opens a package's root
//! resource (`root T`, one per workspace) through the default view of its type, and plans api
//! invocations into [`DynAction`]s. The
//! window widget in `tilolang-process` drives it; so do the collaboration tests, with no window.
//!
//! The runtime is single-threaded. The closures the action api calls are `Send + Sync`, so they
//! hold a [`HostHandle`]: an id that finds its host in a thread-local registry, on the thread
//! that built it. A call from another thread is refused with a message.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value as Json;
use tilolang::{IdsLock, PackageMeta};
use tilolang_runtime::ids::AppId;
use tilolang_runtime::meta::{Meta, Ty, parse_ty};
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::{ResourceRef, Value, from_json};
use tilolang_runtime::{
    Actor, InvokeOutcome, ModuleHash, PackageId, Reading, Runtime, RuntimeError, StoreError,
    ViewInstance,
};

use crate::action::{Authorship, DynAction};
use crate::authority::TiloAuthority;
use crate::meta::MetaExt as _;
use crate::publish::{Caller, PlanRequest, Planner};
use crate::resources::{ResourceStore, Turn};
use crate::schema::lookup;
use tilo_action_policy::MemberAccess;

/// An action's arguments as it runs them, and as its invocation records them; `None` when the
/// line a person typed names no item, so the action does not run.
type PlannedArgs = Option<(Vec<Value>, serde_json::Map<String, Json>)>;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("{0}")]
    Compile(String),
    #[error("{0}")]
    Runtime(#[from] RuntimeError),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    NotFound(String),
}

/// What an app says of one of its actions on one resource now: what it is called there, the
/// icon beside it, and whether it can run on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offered {
    /// What the app calls the action on this resource, for one whose presentation follows its
    /// target (`label: if t.done { "Reopen" } else { "Complete" }`); `None` for one offered as
    /// its package publishes it.
    pub label: Option<String>,
    /// The icon name its presentation gives there.
    pub icon: Option<String>,
    /// Whether it can run on the resource now, and why not; `None` when the app keeps nothing
    /// its target decides, and offers it as it is.
    pub availability: Option<tilo_actions::Availability>,
}

/// What reading `a` and `b` read of the workspace, each field once ([`Reading`]).
fn merged(a: Reading, b: Reading) -> Reading {
    let mut reads = a.reads;
    reads.extend(b.reads);
    reads.sort_unstable();
    reads.dedup();
    Reading {
        reads,
        beyond: a.beyond || b.beyond,
    }
}

/// A loaded package.
#[derive(Clone, Debug)]
pub struct Package {
    pub meta: Meta,
    pub id: PackageId,
    /// The ids lock after compiling, to write back next to the source.
    pub ids: String,
    /// The compiled module, its metadata embedded: what another runtime loads to draw this
    /// package's resources through its own views ([`Host::load_foreign_package`]).
    pub wasm: std::sync::Arc<[u8]>,
}

impl Package {
    /// The view the launcher opens and the type of the resource it opens it over: the package's
    /// first root type (`root T`) and the default view declared for it.
    #[must_use]
    pub fn main(&self) -> Option<(String, String)> {
        let root = self.meta.roots.first()?;
        let view = self.meta.default_view(root)?;
        Some((view.to_owned(), root.clone()))
    }

    /// The type id of the root `name` names: a named root (`root notes: Notes`), or a root by
    /// its type's name or id. `None` when the package declares no such root.
    #[must_use]
    pub fn root_type(&self, name: &str) -> Option<String> {
        if let Some(named) = self.meta.named_roots.iter().find(|root| root.name == name) {
            return Some(named.ty.clone());
        }
        self.meta
            .roots
            .iter()
            .find(|ty| {
                **ty == name
                    || self
                        .meta
                        .types
                        .iter()
                        .any(|t| t.id == **ty && t.name == name)
            })
            .cloned()
    }

    /// The id of the view `name` names: its declared name, or its id. `None` when the package
    /// declares no such view.
    #[must_use]
    pub fn view_id(&self, name: &str) -> Option<String> {
        self.meta
            .views
            .iter()
            .find(|view| view.name == name || view.id == name)
            .map(|view| view.id.clone())
    }
}

/// What one settle of the outbox did ([`Host::settle_outbox`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutboxSettled {
    /// How many changes it wrote, each queued for the API.
    pub written: usize,
    /// It marked an effect as being handed over, which the next settle hands over: whoever
    /// settles owes another settle, without waiting for anything else to happen.
    pub owes_settle: bool,
}

/// The root of a package a view opens over, and the view itself, each chosen by name: a package
/// with more than one root (the shell's `Shell` and its `Layout`) says which layer a view
/// is, rather than taking the first root. `None`s take the package's first root and that
/// type's default view, as the launcher opens an app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootView {
    /// The root, by a named root's name or by its type's name or id.
    pub root: Option<String>,
    /// The view, by its declared name or id.
    pub view: Option<String>,
}

/// A call a run made through the dispatch and waits on: the invocation it went out under, and
/// the run that waits, when the action it was made in runs as one (`None` for a press, whose
/// call the dispatch runs on its own).
pub struct Waiting {
    /// The invocation the child call went out under, as [`Runtime::answer_call`] resumes the
    /// run with its answer.
    pub call: String,
    /// The run that waits on the answer; `None` when the call is the whole of the work.
    pub run: Option<tilolang_runtime::ids::RunId>,
}

pub(crate) struct HostInner {
    handle: u64,
    runtime: Runtime,
    store: ResourceStore,
    authority: TiloAuthority,
    /// The store's reader decision for this workspace, as the runtime's own member reads answer.
    pub(crate) record_reads: std::sync::Arc<crate::members::MemberRecordReads>,
    viewer: Actor,
    packages: RefCell<Vec<Rc<Package>>>,
    /// The calls carried plans wait on, each for its embedder to register and answer.
    pub(crate) suspended: RefCell<std::collections::VecDeque<Waiting>>,
    /// What the run of the action being planned stopped at, for its embedder to register once the
    /// plan is the change the document commits ([`Self::plan`]).
    stopped: RefCell<Option<Waiting>>,
    /// What each action's plan last returned, by the action: the value its invocation returns,
    /// a resource as the reference it made. Publishing what an app offers plans every action, so
    /// one place for the last would give one action another's value; each is kept by its action
    /// and taken when that action's value is asked for ([`Host::take_returned`]).
    returned: RefCell<HashMap<String, Value>>,
    /// When the packages' triggers next have something to run.
    triggers: crate::triggers::Schedule,
    /// Whether a migration waits for its turn: another device runs it, or none may yet
    /// ([`Host::migrate`]).
    migrations_wait: Cell<bool>,
    /// Whether the next plan is the run of an invocation the document applies next
    /// ([`Host::carrying_out`]): what it writes stays, as the change the document commits.
    carried: Cell<bool>,
    /// The person and policy the runtime's members were last given from ([`Host::follow_members`]).
    pub(crate) members_from: RefCell<Option<(String, tilo_action_policy::WorkspacePolicy)>>,
}

thread_local! {
    static LIVE: RefCell<HashMap<u64, Weak<HostInner>>> = RefCell::new(HashMap::new());
}

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

impl Drop for HostInner {
    fn drop(&mut self) {
        let handle = self.handle;
        let _ = LIVE.try_with(|live| live.borrow_mut().remove(&handle));
    }
}

/// The tilolang runtime on the platform. Clones share one runtime.
#[derive(Clone)]
pub struct Host {
    pub(crate) inner: Rc<HostInner>,
}

/// What the API's closures hold in place of the host: see the module docs.
#[derive(Clone)]
pub struct HostHandle {
    id: u64,
    caller: Option<Arc<crate::publish::CallerSource>>,
}

impl std::fmt::Debug for HostHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostHandle")
            .field("id", &self.id)
            .field("caller", &self.caller.is_some())
            .finish()
    }
}

impl HostHandle {
    /// This handle, planning for whoever `source` says is invoking.
    #[must_use]
    pub fn with_caller(
        mut self,
        source: impl Fn() -> Option<Caller> + Send + Sync + 'static,
    ) -> Self {
        self.caller = Some(Arc::new(source));
        self
    }
}

fn with_host<T>(id: u64, run: impl FnOnce(&HostInner) -> Result<T, String>) -> Result<T, String> {
    let host = LIVE
        .with(|live| live.borrow().get(&id).and_then(Weak::upgrade))
        .ok_or_else(|| "the tilolang host is not running on this thread".to_owned())?;
    run(&host)
}

impl Planner for HostHandle {
    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        with_host(self.id, |host| host.plan(request))
    }

    fn caller(&self) -> Option<Caller> {
        self.caller.as_ref().and_then(|source| source())
    }
}

pub(crate) fn value_of(
    host: &HostInner,
    meta: &Meta,
    json: &Json,
    ty: &str,
) -> Result<Value, String> {
    value_of_ty(host, meta, json, &parse_ty(ty, &meta.app)?)
}

/// The value of the type `ty` an argument given as `json` stands for, as the API publishes the
/// parameter ([`crate::surface::published_param`]): a document by the catalogue's reference to it
/// (or its resource id), a plain enum by the name of its case, an actor as the workspace
/// target it is, lists and options as plain JSON. A value encoded as the runtime stores it
/// ([`from_json`]) is taken as it is.
fn value_of_ty(host: &HostInner, meta: &Meta, json: &Json, ty: &Ty) -> Result<Value, String> {
    match (ty, json) {
        (Ty::Record(key), _) => {
            // A parameter may be a type of another app of the workspace: one this host runs, or one
            // it reads a document of (Today's cleanup over the Tasks list it shows). A document of an
            // app this host does not run is read through its own description, and its type is the
            // `<app>#<Type>` a reading is kept under.
            let (kind, type_id) = if key.app == meta.app {
                let declared = meta
                    .type_named(&key.name)
                    .ok_or_else(|| format!("unknown type `{}`", key.name))?;
                (meta.kind_of(&declared.name), declared.id.clone())
            } else if let Some(declared) = host.package_of(&key.app).ok().and_then(|owner| {
                owner
                    .meta
                    .type_named(&key.name)
                    .map(|named| named.id.clone())
            }) {
                (format!("tl.{}.{}", key.app, key.name), declared)
            } else {
                (
                    format!("tl.{}.{}", key.app, key.name),
                    format!("{}#{}", key.app, key.name),
                )
            };
            let text = match json {
                Json::String(text) => text.as_str(),
                Json::Object(reference) => {
                    if reference.get("kind").and_then(Json::as_str) != Some(kind.as_str()) {
                        return Err(format!("a {} is a resource of kind `{kind}`", key.name));
                    }
                    reference
                        .get("id")
                        .and_then(Json::as_str)
                        .ok_or_else(|| format!("a {} reference names its id", key.name))?
                }
                _ => return Err(format!("a {} is given by a reference to it", key.name)),
            };
            // The published id of the resource (`tl:<app>:<id>`), or the id inside the app.
            let id = text
                .strip_prefix(&crate::publish::resource_id(&key.app, ""))
                .unwrap_or(text);
            let target = ResourceRef::parse_id(id, type_id)
                .ok_or_else(|| format!("`{text}` is not a resource id"))?;
            Ok(Value::resource(&target))
        }
        (Ty::Enum(key), Json::String(case)) if key.app == meta.app => {
            let declared = meta
                .enums
                .iter()
                .find(|declared| declared.name == key.name)
                .ok_or_else(|| format!("unknown enum `{}`", key.name))?;
            let variant = declared
                .variants
                .iter()
                .find(|variant| variant.name == *case || variant.id == *case)
                .filter(|variant| variant.fields.is_empty())
                .ok_or_else(|| format!("`{case}` is not a case of {}", key.name))?;
            Ok(Value::Enum(tilolang_runtime::value::EnumValue {
                ty: declared.id.clone(),
                variant: variant.id.clone(),
                fields: std::collections::BTreeMap::new(),
            }))
        }
        (Ty::Actor, Json::Object(target)) if target.len() == 1 => {
            // A member as arguments name them (`{"person": id}`), else an actor as values
            // carry one.
            match serde_json::from_value::<tilolang_runtime::ids::MemberId>(json.clone()) {
                Ok(member) => Ok(Value::Actor(Actor::Member(member))),
                Err(_) => from_json(json).and_then(|value| match value {
                    Value::Actor(actor) => Ok(Value::Actor(actor)),
                    _ => Err("an actor is a person or an agent".to_owned()),
                }),
            }
        }
        // Rich text given as text: its lines as paragraphs.
        (Ty::RichText, Json::String(text)) => Ok(Value::Rich(
            tilolang_runtime::rich::RichValue::from_text(text),
        )),
        (Ty::Option(_), Json::Null) => Ok(Value::none()),
        (Ty::Option(_), Json::Object(encoded))
            if encoded.contains_key("some") || encoded.contains_key("none") =>
        {
            from_json(json)
        }
        (Ty::Option(inner), _) => Ok(Value::some(value_of_ty(host, meta, json, inner)?)),
        (Ty::List(inner) | Ty::Set(inner) | Ty::Collection(inner), Json::Array(items)) => items
            .iter()
            .map(|item| value_of_ty(host, meta, item, inner))
            .collect::<Result<_, _>>()
            .map(Value::List),
        (Ty::Tuple(types), Json::Array(items)) if types.len() == items.len() => types
            .iter()
            .zip(items)
            .map(|(ty, item)| value_of_ty(host, meta, item, ty))
            .collect::<Result<_, _>>()
            .map(Value::Tuple),
        (Ty::Float, Json::Number(number)) => number
            .as_f64()
            .map(Value::Float)
            .ok_or_else(|| "number out of range".to_owned()),
        (Ty::Duration, _) => duration_value(json),
        // A union of documents: the reference's kind says which of them it is.
        (Ty::Union(keys), _) => union_value(host, meta, json, keys),
        _ => from_json(json),
    }
}

/// Whole seconds for a `Duration` parameter, bare or in the boxed scalar form (`{"some": N}`)
/// that a view event or recorded value keeps until `marshal` unboxes it, matching `Int`.
fn duration_value(json: &Json) -> Result<Value, String> {
    let invalid = || "a duration is a whole number of seconds".to_owned();
    match json {
        Json::Number(number) => number.as_i64().map(Value::Int).ok_or_else(invalid),
        Json::Object(encoded) if encoded.len() == 1 => match encoded.get("some") {
            Some(Json::Number(number)) => number
                .as_i64()
                .map(|secs| Value::some(Value::Int(secs)))
                .ok_or_else(invalid),
            _ => Err(invalid()),
        },
        _ => Err(invalid()),
    }
}

/// The document of one of the types `keys` that the reference `json` names by its kind.
fn union_value(
    host: &HostInner,
    meta: &Meta,
    json: &Json,
    keys: &[tilolang_runtime::meta::TypeKey],
) -> Result<Value, String> {
    let Json::Object(reference) = json else {
        return Err(format!(
            "a {} is given by a reference naming its kind",
            union_label(keys)
        ));
    };
    let kind = reference.get("kind").and_then(Json::as_str);
    let key = keys
        .iter()
        .find(|key| Some(crate::surface::resource_type(key).as_str()) == kind)
        .ok_or_else(|| {
            format!(
                "a {} is a resource of kind {}, not {}",
                union_label(keys),
                keys.iter()
                    .map(crate::surface::resource_type)
                    .collect::<Vec<_>>()
                    .join(" or "),
                kind.unwrap_or("none")
            )
        })?;
    value_of_ty(host, meta, json, &Ty::Record(key.clone()))
}

fn union_label(keys: &[tilolang_runtime::meta::TypeKey]) -> String {
    keys.iter()
        .map(|key| key.name.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// `meta` as a runtime that does not run its app loads it: each of its roots (`root Mail`,
/// `root notes: Notes`) named as the resource its app publishes it as, the root's document
/// (`root-<type id>`) by its published id. A root it names already is left as it is.
fn published_roots(meta: &Meta) -> Meta {
    let mut meta = meta.clone();
    for ty in meta.roots.clone() {
        let published = crate::publish::resource_id(&meta.app, &format!("root-{ty}"));
        if let Some(named) = meta.named_roots.iter_mut().find(|root| root.ty == ty) {
            named.published.get_or_insert(published);
        } else {
            // An unnamed root (`root Mail`) is reached by its type alone; its name here
            // is its type's, which no `alias.name` of another package's code reaches.
            let name = meta
                .types
                .iter()
                .find(|declared| declared.id == ty)
                .map_or_else(|| ty.clone(), |declared| declared.name.clone());
            meta.named_roots.push(tilolang::meta::NamedRootMeta {
                name,
                ty,
                published: Some(published),
            });
        }
    }
    meta
}

/// The published id of every resource `value` names, in the order it names them: what an
/// invocation's output lists as the resources it made or revealed.
fn resources_named(app: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Ref(made) => out.push(crate::publish::resource_id(app, &made.target.id_string())),
        Value::List(items) | Value::Tuple(items) => {
            for item in items {
                resources_named(app, item, out);
            }
        }
        Value::Opt(Some(inner)) => resources_named(app, inner, out),
        _ => {}
    }
}

/// A plain enum's case by its name, or an enum with payloads encoded whole: what an action's
/// parameter of the enum `key` of `meta` takes for `chosen`, which `value` holds.
fn enum_json(
    meta: &Meta,
    key: &tilolang_runtime::meta::TypeKey,
    chosen: &tilolang_runtime::value::EnumValue,
    value: &Value,
) -> Result<Json, String> {
    let declared = meta
        .enums
        .iter()
        .find(|declared| declared.name == key.name)
        .ok_or_else(|| format!("unknown enum `{}`", key.name))?;
    let plain = declared
        .variants
        .iter()
        .all(|variant| variant.fields.is_empty());
    match declared
        .variants
        .iter()
        .find(|variant| variant.id == chosen.variant)
    {
        Some(variant) if plain => Ok(Json::String(variant.name.clone())),
        Some(_) => tilolang_runtime::value::to_json(value),
        None => Err(format!(
            "`{}` is not a case of {}",
            chosen.variant, key.name
        )),
    }
}

/// The argument `value` of the type `ty`, as the API takes it: in the shape the action's
/// published parameter declares ([`crate::surface::published_param`]), which the broker binds
/// before it delivers, and which [`value_of`] reads back as the same value. A document by the
/// catalogue's reference to it, of its own type, so a union or a type of another app says which
/// it is; a plain enum by its case's name; an actor as the workspace target it is; an option
/// as its value, or null; lists and tuples item by item; rich text as its text, or encoded
/// whole when it holds more than text. `owner` finds the package of an app the type names.
pub fn argument_json(
    owner: &dyn Fn(&str) -> Result<Meta, String>,
    meta: &Meta,
    value: &Value,
    ty: &Ty,
) -> Result<Json, String> {
    use tilolang_runtime::value::to_json;
    let reference = |key: &tilolang_runtime::meta::TypeKey,
                     target: &ResourceRef|
     -> Result<Option<Json>, String> {
        let found = if key.app == meta.app {
            meta.type_named(&key.name).cloned()
        } else {
            owner(&key.app)?.type_named(&key.name).cloned()
        };
        let declared = found.ok_or_else(|| format!("unknown type `{}`", key.name))?;
        Ok((declared.id == target.ty).then(|| {
            serde_json::json!({
                "app": key.app,
                "kind": crate::surface::resource_type(key),
                "id": target.id_string(),
                "label": "",
            })
        }))
    };
    match (ty, value) {
        (Ty::Record(key), Value::Ref(r)) => reference(key, &r.target)?
            .ok_or_else(|| format!("the resource {} is no {}", r.target.id_string(), key.name)),
        (Ty::Union(keys), Value::Ref(r)) => {
            for key in keys {
                if let Some(json) = reference(key, &r.target)? {
                    return Ok(json);
                }
            }
            Err(format!(
                "the resource {} is no {}",
                r.target.id_string(),
                union_label(keys)
            ))
        }
        (Ty::Option(_), Value::Opt(None)) => Ok(Json::Null),
        (Ty::Option(inner), Value::Opt(Some(present))) => {
            argument_json(owner, meta, present, inner)
        }
        (Ty::Option(inner), present) => argument_json(owner, meta, present, inner),
        (Ty::List(inner) | Ty::Set(inner) | Ty::Collection(inner), Value::List(items)) => items
            .iter()
            .map(|item| argument_json(owner, meta, item, inner))
            .collect::<Result<_, _>>()
            .map(Json::Array),
        (Ty::Tuple(types), Value::Tuple(items)) if types.len() == items.len() => types
            .iter()
            .zip(items)
            .map(|(ty, item)| argument_json(owner, meta, item, ty))
            .collect::<Result<_, _>>()
            .map(Json::Array),
        // A scalar a view carried from an event is boxed until it reaches the action it is passed
        // to; the API takes the plain value `published_param` declares.
        (ty, Value::Opt(Some(present))) if ty.is_scalar() => {
            argument_json(owner, meta, present, ty)
        }
        // A flag travels as the integer the guest boxed it as.
        (Ty::Bool, Value::Int(n)) => Ok(Json::Bool(*n != 0)),
        // A plain enum's parameter chooses among its cases by name
        // ([`crate::surface::published_param`]); one with payloads takes the value encoded.
        (Ty::Enum(key), Value::Enum(chosen)) if key.app == meta.app => {
            enum_json(meta, key, chosen, value)
        }
        // A record value — one built in the code rather than held in a collection — is data, as
        // `json.encode` writes it: its fields under their names, each as the value it holds.
        (Ty::Record(key), Value::Rec(rec)) => {
            let declared = if key.app == meta.app {
                meta.type_named(&key.name).cloned()
            } else {
                owner(&key.app)?.type_named(&key.name).cloned()
            };
            let declared = declared.ok_or_else(|| format!("unknown type `{}`", key.name))?;
            let fields_of = if key.app == meta.app {
                meta.clone()
            } else {
                owner(&key.app)?
            };
            let mut out = serde_json::Map::new();
            for field in &declared.fields {
                let Some(value) = rec.fields.get(&field.id) else {
                    continue;
                };
                let ty = parse_ty(&field.ty, &key.app)?;
                out.insert(
                    field.name.clone(),
                    argument_json(owner, &fields_of, value, &ty)?,
                );
            }
            Ok(Json::Object(out))
        }
        (Ty::Record(_) | Ty::Union(_), other) => Err(format!(
            "a resource is given by a reference to it, not {}",
            to_json(other).unwrap_or(Json::Null)
        )),
        // A person and rich text as every caller gives them.
        (_, Value::Actor(_) | Value::Rich(_)) => tilolang_runtime::value::to_argument(value),
        _ => to_json(value),
    }
}

impl HostInner {
    pub(crate) fn admit_agent(&self, caller: &Caller) {
        self.authority
            .admit_agent(&caller.actor, caller.on_behalf_of.as_deref());
    }

    pub(crate) fn package_of(&self, app: &str) -> Result<Rc<Package>, String> {
        self.packages
            .borrow()
            .iter()
            .find(|package| package.meta.app == app)
            .cloned()
            .ok_or_else(|| format!("no package {app}"))
    }

    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        // Planning is a trial run: whatever it wrote is dropped before the store sees it, so asking
        // for an action (to describe it, to offer it) never changes the workspace. It runs the
        // action, so it is counted: a frame that shows controls plans none. The run of an
        // invocation the document applies next keeps what it wrote: those writes are the change
        // the document commits, and the action runs once.
        let _phase = tilo_resource::profiling::Phase::start("tl.plan");
        let carried = self.carried.replace(false);
        let checkpoint = self.store.checkpoint();
        let planned = self.plan_in_mirror(request);
        let stopped = self.stopped.take();
        if carried && planned.is_ok() {
            self.store
                .keep(checkpoint)
                .map_err(|error| error.to_string())?;
            self.suspended.borrow_mut().extend(stopped);
        } else {
            self.store.rewind(checkpoint);
        }
        planned
    }

    /// The argument `value` of `param` as an invocation records it: as the API takes it, so
    /// the recorded invocation reads back as it ran ([`argument_json`]).
    fn recorded(
        &self,
        meta: &Meta,
        param: &tilolang::meta::ParamMeta,
        value: &Value,
    ) -> Result<Json, String> {
        argument_json(
            &|app| self.package_of(app).map(|package| package.meta.clone()),
            meta,
            value,
            &parse_ty(&param.ty, &meta.app)?,
        )
    }

    /// What `action` returned, for its invocation's value.
    ///
    /// The answer is the action's own: a plan the API made of some other action (publishing a
    /// surface plans each action) writes that action's answer, never this one's, and an
    /// invocation that changed nothing clears only its own. Each answer is taken once
    /// ([`Host::take_returned`]), so one action's plan never answers another's invocation.
    fn returned_by(&self, action: &str, outcome: &InvokeOutcome) {
        let mut returned = self.returned.borrow_mut();
        match outcome {
            // `Unit` is an action that returned nothing, which is no value to give.
            InvokeOutcome::Done(value) if !matches!(value, Value::Unit) => {
                returned.insert(action.to_owned(), value.clone());
            }
            // It waits: what it made is answered when the call it waits on comes back.
            InvokeOutcome::Done(_)
            | InvokeOutcome::Denied
            | InvokeOutcome::Waits { .. }
            | InvokeOutcome::Asked(_) => {
                returned.remove(action);
            }
        }
    }

    /// Whether the action `action` can run on the published resource `resource` now: whether it
    /// can apply to it where a view offers it at all, and the requirements its target alone
    /// decides, as its app says them, in the person at the window's eyes. `None` when the app
    /// keeps neither, or when one cannot be read.
    ///
    /// Asked of the one resource a caller names, when it names it: an action directory answers
    /// the rows it is asked for, an invoked action is refused before it runs, and a window
    /// published the answer for what it shows, so no surface carries a condition's answer for
    /// every item it holds.
    fn availability(
        &self,
        resource: &str,
        action: &str,
        who: &Actor,
    ) -> Option<tilo_actions::Availability> {
        self.offered_reading(resource, action, who)
            .and_then(|(offered, _)| offered.availability)
    }

    /// What the action `action` offers on the published resource `resource` now, as its app says
    /// it: what it is called there and the icon beside it, when the app presents it per target,
    /// whether it can run on it now — and what reading all of that read of the workspace, so a
    /// caller keeping the answer asks again only when one of those fields moves ([`Reading`]).
    /// `None` when the app presents nothing of the action and keeps no requirement it can
    /// decide, or when one cannot be read.
    fn offered_reading(
        &self,
        resource: &str,
        action: &str,
        who: &Actor,
    ) -> Option<(Offered, Reading)> {
        // What asking reads of the app's package: the action's declaration and the type of its
        // target. Copied out of the registry by value, not the package: a package is the whole
        // app's compiled metadata, and a window asks this once for each action of each row it
        // shows, so cloning it here would cost more than reading the requirements.
        let (app, has_guard, has_applies, has_present, declared) = {
            let packages = self.packages.borrow();
            let package = packages
                .iter()
                .find(|package| package.meta.actions.iter().any(|held| held.id == action))?;
            let meta = package.meta.actions.iter().find(|held| held.id == action)?;
            let declared = package.meta.target_of(&meta.params)?.id.clone();
            (
                package.meta.app.clone(),
                meta.guard,
                meta.applies,
                meta.present.is_some(),
                declared,
            )
        };
        if !has_guard && !has_applies && !has_present {
            return None;
        }
        let id = resource
            .strip_prefix(&crate::publish::resource_id(&app, ""))
            .unwrap_or(resource);
        let target = ResourceRef::parse_id(id, declared)?;
        // What deciding the action read: what it shows, what applies decided, and what its
        // requirement did.
        let mut read = Reading::default();
        let mut offered = Offered::default();
        if has_present {
            // What it is called and the icon beside it on this resource, as its presentation
            // gives them now: a menu of a finished task offers "Reopen", not "Complete".
            match self.runtime.presentation_reading(action, &target, who) {
                Ok((presented, read_here)) => {
                    if let Some(presented) = presented {
                        offered.label = presented.label;
                        offered.icon = presented.icon;
                    }
                    read = read_here;
                }
                // A presentation that cannot be read says nothing of the action: it is offered
                // as its package publishes it.
                Err(_) => return None,
            }
        }
        // Where the action cannot apply at all, the offer leaves it out: that reason is the
        // answer, before what it needs to run.
        let decided = |reason: Option<String>| match reason {
            Some(reason) => tilo_actions::Availability::Unavailable {
                code: "unmet".to_owned(),
                reason,
            },
            None => tilo_actions::Availability::Available,
        };
        if has_applies {
            match self.runtime.applies_reading(action, &target, who) {
                Ok((Some(reason), applies)) => {
                    offered.availability = Some(decided(Some(reason)));
                    return Some((offered, merged(read, applies)));
                }
                Ok((None, applies)) => read = merged(read, applies),
                // A condition that cannot be read says nothing of the action: it is offered,
                // and refuses with its reason when it runs.
                Err(_) => return None,
            }
        }
        if !has_guard {
            offered.availability = Some(decided(None));
            return Some((offered, read));
        }
        match self.runtime.refusal_reading(action, &target, who) {
            Ok((reason, guard)) => {
                offered.availability = Some(decided(reason));
                Some((offered, merged(read, guard)))
            }
            Err(_) => None,
        }
    }

    fn plan_in_mirror(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        let package = self.package_of(request.app)?;
        // A bound control's edit of a field is no declared action: it writes the field itself.
        if let Some(edit) = crate::publish::plan_edit(
            &self.store,
            &package.meta,
            request,
            &request
                .caller
                .map_or_else(|| self.viewer.clone(), |caller| caller.actor.clone()),
        ) {
            return edit;
        }
        // A scenario is a fixture: only a seed runs it, straight on the runtime. Through the
        // API, as a person, an agent or another app calls, it is no action.
        let action = package
            .meta
            .actions
            .iter()
            .find(|action| action.id == request.action_id && !action.scenario)
            .ok_or_else(|| format!("no action {}", request.action_id))?;
        // The runtime sees the invoker the API admitted: an agent's task delegate for itself,
        // the person at the window otherwise.
        let invoker = request
            .caller
            .map_or_else(|| self.viewer.clone(), |caller| caller.actor.clone());
        // A requirement is the app's, and the app is what can read it: an action published
        // while its rule held, or one an agent addresses by name, is refused here, before it
        // runs, with its rule's reason, decided for the invoker it then runs as.
        if let Some(target) = request.resource
            && let Some(tilo_actions::Availability::Unavailable { reason, .. }) =
                self.availability(target, request.action_id, &invoker)
        {
            return Err(reason);
        }
        if let Some(caller) = request.caller.filter(|caller| caller.agent) {
            self.admit_agent(caller);
        }
        // The records an agent makes belong to the person it acts for: a type that names
        // `readers: author` seals them to that person, not to the agent's own membership.
        let _acting_for = self.store.acting_for(
            request
                .caller
                .and_then(|caller| caller.on_behalf_of.clone()),
        );
        let recorded = match self.planned_args(&package, action, request)? {
            Some((args, recorded)) => {
                self.store.take_made();
                // The API admitted the call before it asked for its plan, asking the person
                // first where it had to: the run does not ask them again.
                let outcome = self
                    .runtime
                    .invoke_admitted_through(
                        &invoker,
                        request.caller.and_then(|caller| caller.app.as_deref()),
                        (request.action_id, args),
                        request.from,
                    )
                    .map_err(|error| error.to_string())?;
                self.returned_by(request.action_id, &outcome);
                match outcome {
                    InvokeOutcome::Done(_) => {}
                    // It waits on a call it made through the dispatch: the run goes on with the
                    // answer, and the plan is the change it wrote before waiting. A press's call
                    // answers nobody, and the dispatch runs it.
                    InvokeOutcome::Waits { run, call } => {
                        self.stopped.replace(Some(Waiting {
                            call,
                            run: Some(run),
                        }));
                    }
                    // It needs a person's yes first: the call went through the dispatch, which
                    // records it as an asked invocation and answers with the result.
                    InvokeOutcome::Asked(call) => {
                        self.stopped.replace(Some(Waiting { call, run: None }));
                    }
                    InvokeOutcome::Denied => return Err("the action is not allowed".to_owned()),
                }
                recorded
            }
            // The one line typed names nothing — only space and what the action's other
            // parameters take — so the action does not run and makes no change.
            None => {
                request.args.clone()
            }
        };
        Ok(DynAction {
            app: request.app.to_owned(),
            kind: request.kind.to_owned(),
            action_id: request.action_id.to_owned(),
            args: Json::Object(recorded),
            authorship: Authorship {
                invoker: Some(invoker),
                chain: Vec::new(),
                module: package.id.0.clone(),
                at: None,
                from: request.from.cloned(),
                suggested_by: None,
                count: 1,
                invocation: None,
                gesture: None,
                readers: None,
            },
        })
    }

    /// The action's arguments as its invocation records them: those the API was given, and
    /// each left out that has a default computed now, on the invoker's state, since what the
    /// person saw is the argument. `None` when the line typed names no item, so the action
    /// does not run at all.
    fn planned_args(
        &self,
        package: &Package,
        action: &tilolang::meta::ActionMeta,
        request: &PlanRequest<'_>,
    ) -> Result<PlannedArgs, String> {
        let mut given: Vec<Option<Value>> = Vec::new();
        let mut params = action.params.iter();
        // What the action acts on: the first parameter's type, or, when it takes a union of
        // types (`to: TextChannel | Thread | Direct`), the case the invoked resource is of, as
        // the row the action was invoked on names its kind.
        let case = package.meta.target_of(&action.params).or_else(|| {
            let name = request
                .kind
                .strip_prefix(&format!("tl.{}.", package.meta.app))?;
            package.meta.type_named(name)
        });
        if let (Some(target), Some(declared)) = (request.resource, case) {
            let target = ResourceRef::parse_id(target, declared.id.clone())
                .ok_or_else(|| format!("`{target}` is not a resource id"))?;
            given.push(Some(Value::resource(&target)));
            params.next();
        }
        // What a person typed as one line, when the action takes an entry and the API was
        // given it: the words it holds fill the parameters they name, and the entry takes the
        // rest, so one line from a palette makes the same task as one from the app's entry.
        // It is the call's only argument: a call that names its other parameters was given
        // them, and reads its entry as the text it is, not as a line to fill the rest from.
        let entry = action.params.iter().find(|param| param.entry);
        let entry_text = entry
            .filter(|_| request.args.len() == 1)
            .and_then(|param| request.args.get(&param.name))
            .and_then(Json::as_str);
        let mut computed = Vec::new();
        for param in params {
            if entry_text.is_some() && entry.is_some_and(|entry| entry.name == param.name) {
                computed.push(param);
                given.push(None);
                continue;
            }
            match request.args.get(&param.name) {
                None if param.default.is_some() => {
                    computed.push(param);
                    given.push(None);
                }
                json => {
                    let json = json.cloned().unwrap_or(Json::Null);
                    given.push(Some(value_of(self, &package.meta, &json, &param.ty)?));
                }
            }
        }
        if let Some(text) = entry_text {
            let Some(filled) = self
                .runtime
                .fill_entry(request.action_id, given, text)
                .map_err(|error| error.to_string())?
            else {
                return Ok(None);
            };
            given = filled;
        }
        let args = self
            .runtime
            .complete_args(request.action_id, given)
            .map_err(|error| error.to_string())?;
        let mut recorded = request.args.clone();
        for param in computed {
            let index = action
                .params
                .iter()
                .position(|declared| declared.name == param.name);
            if let Some(value) = index.and_then(|index| args.get(index)) {
                let json = self.recorded(&package.meta, param, value)?;
                recorded.insert(param.name.clone(), json);
            }
        }
        Ok(Some((args, recorded)))
    }
}

impl Host {
    /// A host for `viewer` over a store of its own, in memory: a device with no workspace, and
    /// the tests. `access` reads the viewer's member access in the workspace: what the runtime
    /// invokes is decided by the workspace's policy ([`TiloAuthority`]).
    ///
    /// # Errors
    /// The runtime cannot be set up, which is a bug.
    pub fn new(
        viewer: Actor,
        access: impl Fn() -> Option<MemberAccess> + 'static,
    ) -> Result<Self, HostError> {
        Self::open(ResourceStore::in_memory(), viewer, None, access)
    }

    /// A host for `viewer` over `store`, on the device `device`: the device that carries out the
    /// outside effects this host queues. A host without one is a device of its own.
    ///
    /// # Errors
    /// The runtime cannot be set up, which is a bug.
    pub fn open(
        store: ResourceStore,
        viewer: Actor,
        device: Option<&str>,
        access: impl Fn() -> Option<MemberAccess> + 'static,
    ) -> Result<Self, HostError> {
        let _ = device;
        // The host's own record types (`Invocation`) are stored like any other resource.
        let host_meta = tilolang::host_meta();
        host_meta.register_schemas();
        store.declare(&host_meta);
        store.set_writer(viewer.clone());
        // A reader field is placed by the workspace's one read decision, the same check the
        // runtime's members are given: the store keeps no policy of its own.
        let record_reads = std::sync::Arc::new(crate::members::MemberRecordReads::default());
        record_reads.follow(access(), &viewer);
        store.set_record_reader(record_reads.clone());
        // What the runtime invokes is decided by the workspace's one policy: the person's access,
        // an agent's delegation and the answers the person gave it, and the code's authors.
        let authority = TiloAuthority::new(viewer.clone(), access);
        let runtime = Runtime::new(store.clone(), authority.clone())?;
        runtime.set_clock(system_time);
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        let triggers = crate::triggers::Schedule::new(store.watch());
        let inner = Rc::new(HostInner {
            handle,
            runtime,
            store,
            authority,
            record_reads,
            viewer,
            packages: RefCell::new(Vec::new()),
            suspended: RefCell::default(),
            stopped: RefCell::default(),
            returned: RefCell::default(),
            triggers,
            migrations_wait: Cell::new(false),
            carried: Cell::new(false),
            members_from: RefCell::new(None),
        });
        LIVE.with(|live| live.borrow_mut().insert(handle, Rc::downgrade(&inner)));
        let host = Self { inner };
        // What an app reads of the workspace's members stands from the first frame: the policy
        // the host decides by (or the empty list of a host that has none yet).
        host.follow_members();
        Ok(host)
    }

    /// Replace what `now` reads (the system time until then): tests and captures fix it. The
    /// clock answers ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn set_clock(&self, clock: impl Fn() -> String + 'static) {
        let clock = Rc::new(clock);
        self.inner.store.set_now({
            let clock = Rc::clone(&clock);
            move || clock()
        });
        self.inner.runtime.set_clock(move || clock());
    }

    /// Where the person is, in seconds east of UTC (until then: 0): what `today` reads.
    pub fn set_zone(&self, zone: impl Fn() -> i64 + 'static) {
        self.inner.runtime.set_zone(zone);
    }

    /// Where the words packages and the platform show come from, in the person's language
    /// (`phrase(...)`, declarations' labels and nouns, and what a reference to a resource they
    /// do not hold says wherever rich text is read for them).
    pub fn set_words(&self, words: impl crate::runtime::Words + 'static) {
        self.inner.runtime.set_words(words);
    }

    /// The handle the API's closures hold.
    #[must_use]
    pub fn handle(&self) -> HostHandle {
        HostHandle {
            id: self.inner.handle,
            caller: None,
        }
    }

    /// Admit `caller` as software acting for a person: a task. It acts with that person's
    /// authority bounded by its delegation and the answers the person gave it in the workspace; its
    /// code is trusted as far as that goes.
    pub fn admit_agent(&self, caller: &Caller) {
        self.inner.admit_agent(caller);
    }

    /// The next call a carried plan waits on, with the run that waits on it (`None` when the
    /// call is the whole of the work): what the embedder registers so the answer resumes it.
    #[must_use]
    pub fn take_waiting(&self) -> Option<Waiting> {
        self.inner.suspended.borrow_mut().pop_front()
    }

    /// Whether the outbox holds an outside effect this device has yet to carry out.
    #[must_use]
    pub fn outbox_waits(&self) -> bool {
        !self.inner.store.outbox_pending().is_empty()
    }

    /// Carry out the outside effects of this device that wait in the document
    /// ([`Runtime::settle`]): each is marked as being handed over, then goes to the sink, and
    /// what became of it is queued for the API like any other change. What `delivery(r)`
    /// reads follows the document either way.
    ///
    /// # Errors
    /// No sink is set, or the store failed.
    pub fn settle_outbox(&self) -> Result<OutboxSettled, String> {
        if !self.outbox_waits() {
            self.inner.runtime.refresh_deliveries();
            return Ok(OutboxSettled::default());
        }
        self.inner
            .runtime
            .settle()
            .map(|report| OutboxSettled {
                written: [
                    report.failed.len(),
                    report.marked,
                    report.waiting,
                    report.unknown,
                ]
                .into_iter()
                .fold(report.delivered, usize::saturating_add),
                owes_settle: report.marked > 0,
            })
            .map_err(|error| error.to_string())
    }

    /// The runtime, for blocks, programs and the catalogue.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.inner.runtime
    }

    /// Tell the host the Rust views this process declares, so a place showing a resource of one
    /// of their types draws that app's view, exactly as it draws a tilolang package's view
    /// (one-app-model.md §2). Everything a `#[view]` linked into this process declared
    /// ([`crate::views::DeclaredViews`]).
    pub fn declare_rust_views(&self) {
        if !tilo_view::ViewRegistry::is_empty() {
            self.inner
                .runtime
                .set_rust_views(std::rc::Rc::new(crate::views::DeclaredViews));
        }
    }

    #[must_use]
    pub fn store(&self) -> &ResourceStore {
        &self.inner.store
    }

    #[must_use]
    pub fn authority(&self) -> &TiloAuthority {
        &self.inner.authority
    }

    /// Whether the action published as `action` can run on the published resource `resource`
    /// now, as its app says: the requirements its target alone decides hold for the person at the window, or the
    /// first that does not and why. `None` when the app keeps no requirement its target could
    /// decide, and the action is offered as it is.
    #[must_use]
    pub fn availability(&self, resource: &str, action: &str) -> Option<tilo_actions::Availability> {
        let id = self.published_action(resource, action)?.id;
        self.inner.availability(resource, &id, &self.inner.viewer)
    }

    /// What the action published as `action` offers on the published resource `resource` now, as
    /// its app says it ([`Offered`]), with what reading the answer read of the workspace: the fields it read,
    /// each once, so a caller keeping the answer knows what has to move for it to be read again,
    /// and whether it read beyond the workspace at all ([`Reading`]). `None` when the app
    /// presents nothing of the action and keeps no requirement it can decide, or when one cannot
    /// be read.
    #[must_use]
    pub fn offered_reading(&self, resource: &str, action: &str) -> Option<(Offered, Reading)> {
        let id = self.published_action(resource, action)?.id;
        self.inner
            .offered_reading(resource, &id, &self.inner.viewer)
    }

    /// The action the API publishes as `action` on `resource`: one of the package whose
    /// resource it is (`tl:<app>:<id>`, `tl:<app>`), by the name the app publishes it under or
    /// the id its declaration keeps
    /// ([`MetaExt::action_published_as`](crate::meta::MetaExt::action_published_as)).
    #[must_use]
    pub fn published_action(
        &self,
        resource: &str,
        action: &str,
    ) -> Option<tilolang_runtime::meta::ActionMeta> {
        let app = crate::publish::app_of(resource)?;
        self.find_in_packages(|package| {
            if package.meta.app != app {
                return None;
            }
            // An action's name belongs to its resource type: where two of the package's types
            // each have an action of this name, the one of the resource it is called on.
            let named: Vec<&tilolang_runtime::meta::ActionMeta> = package
                .meta
                .actions
                .iter()
                .filter(|declared| !declared.scenario && declared.name == action)
                .collect();
            if named.len() < 2 {
                return package.meta.action_published_as(action).cloned();
            }
            let id = resource.strip_prefix(&crate::publish::resource_id(app, ""))?;
            let ty = self.store().type_of_id(id)?;
            let name = &package.meta.types.iter().find(|t| t.id == ty)?.name;
            named
                .into_iter()
                .find(|declared| declared.params.first().is_some_and(|p| p.ty == *name))
                .cloned()
        })
    }

    /// Run `encode`, the API building an invocation that is carried out as soon as it is built:
    /// the action it plans keeps what it wrote in the store, so the action does not run a second
    /// time. Every other plan is a trial and is taken back.
    pub fn carrying_out<R>(&self, author: &crate::runtime::Actor, encode: impl FnOnce() -> R) -> R {
        self.inner.carried.set(true);
        let _ = author;
        let out = encode();
        self.inner.carried.set(false);
        out
    }

    /// What `action` returned when it was last planned here, for whoever invoked it, once: the
    /// value its invocation carries, a resource as the reference it made.
    #[must_use]
    pub fn take_returned(&self, action: &str) -> Option<Value> {
        self.inner.returned.borrow_mut().remove(action)
    }

    /// What `action` of `app` returned as `value`, as the output of its invocation: the value
    /// its declared result carries as data ([`crate::publish::result_ty`]), and the resource it
    /// made, by its published id.
    ///
    /// A value of a type the API carries no data for (a file, the bytes of a response) names
    /// the resource it made alone: the app shows it rather than reading it out.
    ///
    /// # Errors
    /// The value is not of the action's declared result type, or a type it names is in a package
    /// this host does not hold: it cannot be read as what the action declared.
    pub fn action_output(
        &self,
        app: &str,
        action: &str,
        value: &Value,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let package = self.inner.package_of(app)?;
        let declared = package
            .meta
            .actions
            .iter()
            .find(|declared| declared.id == action)
            .ok_or_else(|| format!("no action {action} in {app}"))?;
        let mut output = tilo_actions::ActionOutput::default();
        resources_named(app, value, &mut output.resources);
        if let Some(ty) = crate::publish::result_ty(&package.meta, declared) {
            let owner = |app: &str| {
                self.inner
                    .package_of(app)
                    .map(|package| package.meta.clone())
            };
            output.value = Some(argument_json(&owner, &package.meta, value, &ty)?);
        }
        Ok(output)
    }

    #[must_use]
    pub fn viewer(&self) -> &Actor {
        &self.inner.viewer
    }

    /// Compile `source` against the packages already loaded and the platform's packages it names,
    /// register its schemas, load its module and trust it (a built-in package). A package names
    /// another app's published types (Contacts' directory, a device's roots) without that app
    /// being loaded here, as it does when the workspace compiles a built-in one.
    pub fn load_builtin(&self, source: &str, ids: &str) -> Result<Package, HostError> {
        let lock = IdsLock::parse(ids).map_err(|error| HostError::Compile(error.to_string()))?;
        let named =
            crate::platform_apps::named_platform_packages(source).map_err(HostError::Compile)?;
        let mut imports = named.clone();
        imports.extend(
            self.inner
                .packages
                .borrow()
                .iter()
                .map(|package| package.meta.clone()),
        );
        let compiled =
            tilolang::compile_package(source, &lock, &imports).map_err(|diagnostics| {
                HostError::Compile(
                    diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.render("app.tl"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            })?;
        let package = self.load_compiled(&compiled)?;
        // The platform apps the package names are described here too, without their code: their
        // types make the references the package writes real (`workspace.members`), and what a
        // view reads of them comes from the readings this host serves. An app this host runs
        // already (the workspace's own service) keeps its package.
        for meta in named {
            if meta.app != package.meta.app
                && self.package(&meta.app).is_none()
                && let Err(error) = self.inner.runtime.load_foreign(meta)
            {
                tracing::warn!(%error, "a platform package the app names is not described here");
            }
        }
        Ok(package)
    }

    /// Load a compiled package and trust it (a built-in package, or the package being developed).
    /// A package of the same app that is already loaded is replaced: its resources keep their
    /// state, and views opened over the old version must be opened again to run the new code.
    pub fn load_compiled(&self, compiled: &tilolang::Compiled) -> Result<Package, HostError> {
        self.load_module(
            &compiled.wasm,
            compiled.meta.clone(),
            compiled.ids.to_text(),
        )
    }

    /// Load the package an installed app is, after the packages it uses: their views draw and
    /// their actions run on the resources of theirs it holds (a board's todo rows), each
    /// package under its own authority, as if it ran on its own.
    pub fn load_bundle(&self, bundle: &crate::bundle::Bundle) -> Result<Package, HostError> {
        self.load_bundle_using(bundle, |_| false)
    }

    /// [`Self::load_bundle`], with each package it uses for which `other` holds loaded as
    /// another app's ([`Self::load_foreign_package`]): an app of its own, whose documents stay in
    /// its own document and are read from there, and whose actions run at its own api.
    pub fn load_bundle_using(
        &self,
        bundle: &crate::bundle::Bundle,
        other: impl Fn(&str) -> bool,
    ) -> Result<Package, HostError> {
        for import in &bundle.imports {
            if other(&import.meta.app) {
                self.load_foreign_package(&import.wasm, &import.meta)?;
            } else {
                self.load_module(&import.wasm, import.meta.clone(), String::new())?;
            }
        }
        self.load_module(&bundle.wasm, bundle.meta.clone(), bundle.ids.clone())
    }

    /// Load a package from its compiled module alone, with the metadata the module embeds, and
    /// trust it: how a launcher starts a built-in package compiled when Tilo was built, so that
    /// launching compiles no tilolang. The package has no ids lock to write back.
    pub fn load_wasm(&self, wasm: &[u8]) -> Result<Package, HostError> {
        let meta = PackageMeta::from_wasm(wasm).ok_or_else(|| {
            HostError::Compile("the module embeds no tilolang metadata".to_owned())
        })?;
        self.load_module(wasm, meta, String::new())
    }

    /// Load a compiled module with its metadata and trust it; see [`Self::load_compiled`].
    pub fn load_module(&self, wasm: &[u8], meta: Meta, ids: String) -> Result<Package, HostError> {
        let id = self.inner.runtime.reload_package(wasm, meta.clone())?;
        meta.register_schemas();
        // The ids the package's lock retired are declarations this build carries the same id as
        // under another name: what an earlier version wrote under them is read as this one.
        let former: Vec<(String, String)> = tilo_failure::ignored!(
            "a package with no readable ids lock retired no ids",
            IdsLock::parse(&ids).map(|lock| {
                lock.retired()
                    .map(|(name, id)| (name.to_owned(), id.to_owned()))
                    .collect()
            })
        )
        .unwrap_or_default();
        self.inner.store.declare_with(&meta, &former);
        // The package's own table too: an action applied or described outside this store (a
        // receiver replaying it, a task's api) resolves its record's type by this version.
        crate::schema::register_package(&meta, &id);
        // A package loaded from the module compiled into this build ships no surface file of its
        // own: the host generates it from the metadata, as `tilolang build` writes it for an
        // installed app, so the changes it makes name a surface this process decides them
        // against. A package that named its own surface keeps it.
        if collab_types::app_surface::hash_of(&meta.app).is_none() {
            let reads: Vec<Meta> = meta
                .uses
                .iter()
                .filter_map(|app| self.package(app).map(|package| package.meta))
                .collect();
            if let Ok(bytes) = crate::surface::surface_toml_reading(
                &[&meta],
                &reads,
                &tilolang_runtime::SourceWords,
            ) {
                collab_types::app_surface::register_package(&meta.app, &bytes);
            }
        }
        self.inner.authority.load(&id, &meta);
        let package = Rc::new(Package {
            meta,
            id,
            ids,
            wasm: std::sync::Arc::from(wasm),
        });
        self.inner.triggers.forget();
        {
            let mut packages = self.inner.packages.borrow_mut();
            match packages
                .iter_mut()
                .find(|loaded| loaded.meta.app == package.meta.app)
            {
                Some(loaded) => *loaded = Rc::clone(&package),
                None => packages.push(Rc::clone(&package)),
            }
        }
        // The package's kinds place its reader fields at their own nodes from now on.
        self.inner.record_reads.follow_with_kinds(
            self.inner.authority.access(),
            self.viewer(),
            crate::members::kind_nodes(self),
        );
        Ok(Package::clone(&package))
    }

    /// Load the package of an app this host does not run, trusted: its views draw resources of
    /// that app from the readings the session hands in ([`tilolang_runtime::Runtime::set_reading`]),
    /// and what they change is sent to those resources' apis through the runtime's invoker.
    /// The store keeps none of its documents, so the package is not among [`Self::packages`];
    /// its code is decided by the host's authority as the app's own, as a loaded package's is.
    ///
    /// # Errors
    /// The module does not link.
    ///
    /// Its views read its roots as they do in its own app (`let m = root Mail`): each root is
    /// the resource its app publishes it as ([`crate::publish::resource_id`]), whose reading
    /// the session hands in as it does any other of that app's.
    pub fn load_foreign_package(&self, wasm: &[u8], meta: &Meta) -> Result<(), HostError> {
        let meta = published_roots(meta);
        let id = self
            .inner
            .runtime
            .load_foreign_package(wasm, meta.clone())?;
        self.inner.authority.load(&id, &meta);
        Ok(())
    }

    /// Unload the package of the app `app` this host does not run, once nothing here shows
    /// any of its resources: its views run here no more, and the readings of its resources
    /// are forgotten. Whether it was loaded.
    pub fn unload_foreign(&self, app: &str) -> bool {
        let unloaded = self.inner.runtime.unload_foreign(app);
        if unloaded {
            self.inner.authority.forget_app(app);
        }
        unloaded
    }

    /// Every loaded package.
    #[must_use]
    pub fn packages(&self) -> Vec<Rc<Package>> {
        self.inner.packages.borrow().clone()
    }

    /// The ids of the loaded packages, in the order they loaded: what a publisher compares to
    /// know the host runs other packages now.
    #[must_use]
    pub fn package_ids(&self) -> Vec<tilolang_runtime::PackageId> {
        self.inner
            .packages
            .borrow()
            .iter()
            .map(|package| package.id.clone())
            .collect()
    }

    /// The loaded package `app`.
    #[must_use]
    pub fn package(&self, app: &str) -> Option<Package> {
        self.inner
            .package_of(app)
            .ok()
            .map(|package| Package::clone(&package))
    }

    /// The first thing `find` finds in a loaded package, read where the packages are kept: what
    /// a frame looks up (an action's meta, an enum's cases) without copying every package.
    /// `find` must not load or drop a package.
    pub fn find_in_packages<R>(&self, mut find: impl FnMut(&Package) -> Option<R>) -> Option<R> {
        self.inner
            .packages
            .borrow()
            .iter()
            .find_map(|package| find(package))
    }

    /// The package's root resource: the one per-workspace resource of its root type, created the
    /// first time it is wanted: its first root type with a default view, as the launcher opens
    /// an app.
    pub fn root_of(&self, app: &str) -> Result<ResourceRef, HostError> {
        self.root_of_layer(app, &RootView::default())
    }

    /// The root `chosen` names in `app`, created the first time it is wanted
    /// ([`RootView::default`] takes the package's first root).
    pub fn root_of_layer(&self, app: &str, chosen: &RootView) -> Result<ResourceRef, HostError> {
        let ty = self.root_type_of(app, chosen)?;
        self.root_resource(app, &ty)
    }

    /// The package's root resource of the root type `ty` (a type id): what a named root
    /// (`root shell: Shell`) is, by the id the checker gives it, or what
    /// [`Self::root_of_layer`] names. Created the first time it is wanted, as
    /// [`Self::root_of`] does.
    pub fn root_resource(&self, app: &str, ty: &str) -> Result<ResourceRef, HostError> {
        let app_is_loaded = self
            .find_in_packages(|package| (package.meta.app == app).then_some(()))
            .is_some();
        if !app_is_loaded {
            return Err(HostError::NotFound(format!("package {app}")));
        }
        let id = format!("root-{ty}");
        let root = ResourceRef::root(id, ty.to_owned());
        if self.inner.store.contains(&root) {
            return Ok(root);
        }
        // Made with what its type starts with, in one change that is nobody's: what a package
        // fills its first start with is there before the window that asked for the root is
        // drawn, and nothing about it is anyone's to catch up on.
        Ok(self.inner.runtime.create_root(ty)?)
    }

    /// The type id of the root `chosen` names in `app`: the named root, or a root by its type's
    /// name or id, or the package's first root with a default view when none is named.
    ///
    /// # Errors
    /// The package is not loaded, names no such root, or its first root has no default view.
    fn root_type_of(&self, app: &str, chosen: &RootView) -> Result<String, HostError> {
        let package = self
            .package(app)
            .ok_or_else(|| HostError::NotFound(format!("package {app}")))?;
        if let Some(name) = &chosen.root {
            return package
                .root_type(name)
                .ok_or_else(|| HostError::NotFound(format!("a root `{name}` in {app}")));
        }
        package
            .main()
            .map(|(_, ty)| ty)
            .ok_or_else(|| HostError::NotFound(format!("a root type with a default view in {app}")))
    }

    /// Run each loaded package's migrations (`migration action`): once on each resource of the
    /// migration's target type that an earlier version of the package wrote (one this version
    /// makes is marked migrated in its making), as an action of the package version
    /// (the app acting by itself), which records in the same change that it
    /// ran. A field the new build keeps as `RichText` where the old one kept `Text` has its
    /// text carried into rich text first, the same words ([`crate::store`]'s
    /// `upgrade_plain_text`). Each migration of a resource runs once in the workspace, on the
    /// device whose turn it is ([`ResourceStore::take_turn`]): one another device runs, or none
    /// may run yet, waits ([`Self::migrations_wait`]). Returns how many migrations ran here. A
    /// migration that is refused or fails leaves its resource as it was, to be tried again the
    /// next time.
    pub fn migrate(&self) -> usize {
        self.inner.migrations_wait.set(false);
        // A package that takes over what a native app of its id kept carries it into its root
        // on this start: the root is made for it (not marked migrated, since what it starts as
        // is what that app left), and its migrations run on it below.
        for package in self.packages() {
            if !self.inner.runtime.has_earlier(&package.meta.app) {
                continue;
            }
            for migration in package.meta.migrations() {
                let Some(ty) = migration
                    .params
                    .first()
                    .and_then(|target| package.meta.type_named(&target.ty))
                else {
                    continue;
                };
                if package.root_type(&ty.name).is_some()
                    && let Err(error) = self.root_resource(&package.meta.app, &ty.id)
                {
                    tracing::warn!(app = package.meta.app, %error, "the root to migrate into could not be made");
                }
            }
        }
        let mut ran: usize = 0;
        for package in self.packages() {
            for migration in package.meta.migrations() {
                let Some(ty) = migration
                    .params
                    .first()
                    .and_then(|target| package.meta.type_named(&target.ty))
                else {
                    continue;
                };
                for doc in self.inner.store.resources_of(&ty.id).unwrap_or_default() {
                    ran = ran.saturating_add(self.migrate_doc(&package, &ty.id, &doc));
                }
            }
        }
        ran
    }

    /// Do each of `package`'s migrations of the type `ty` (a type id) that has not happened on
    /// the resource `doc`, as the package's own invocation. Answers how many happened.
    fn migrate_doc(&self, package: &Package, ty: &str, doc: &str) -> usize {
        let mut ran: usize = 0;
        for migration in package.meta.migrations() {
            let Some(target) = migration
                .params
                .first()
                .and_then(|target| package.meta.type_named(&target.ty))
            else {
                continue;
            };
            let target_ref = ResourceRef::root(doc, ty.to_owned());
            if target.id != ty || self.inner.store.migrated(&target_ref, &migration.id) {
                continue;
            }
            // A migration that carries what a native app of the package's id kept waits until
            // this device holds some of it: finding nothing proves only that nothing has arrived
            // yet, so it stays to run on a start that reads what the documents brought.
            if migration.carries && !self.inner.runtime.has_earlier(&package.meta.app) {
                continue;
            }
            let at = Value::resource(&target_ref);
            // The migration of this resource is the workspace's once: the device whose turn it is
            // runs it, and the mark it writes in the resource tells every device it ran.
            let address = format!("{}/{doc}#{}", package.meta.app, migration.id);
            match self.inner.store.take_turn(&address, &Json::Null) {
                Turn::Take => {}
                Turn::Done(_) => continue,
                Turn::Later => {
                    self.inner.migrations_wait.set(true);
                    continue;
                }
            }
            // The package updating its own documents, not the person at the window: the change
            // is the package version's, under the platform's migration actor.
            let by = Actor::App(AppId::from(package.meta.app.as_str()));
            self.inner.store.migrating(&target_ref, &migration.id);
            let outcome = self
                .inner
                .runtime
                .invoke_action(&by, (migration.id.as_str(), vec![at]));
            let done = matches!(outcome, Ok(InvokeOutcome::Done(_)));
            if let Err(error) = self
                .inner
                .store
                .migrated_by(&target_ref, &migration.id, &by, done)
            {
                tracing::warn!(%error, "a migration that ran is not marked");
            }
            match outcome {
                Ok(InvokeOutcome::Done(answer)) => {
                    self.inner.store.turn_done(
                        &address,
                        &Json::Null,
                        crate::triggers::answer_json(&answer),
                    );
                    ran = ran.saturating_add(1);
                }
                other => tracing::warn!(
                    app = package.meta.app,
                    migration = migration.name,
                    doc,
                    ?other,
                    "a migration did not run"
                ),
            }
        }
        ran
    }

    /// Whether a migration [`Self::migrate`] met waits for its turn: it is worth running again
    /// a little later.
    #[must_use]
    pub fn migrations_wait(&self) -> bool {
        self.inner.migrations_wait.get()
    }

    /// Run what the packages declare comes due at `now` (seconds since the epoch): each
    /// trigger, for each document of its type, as the app's own invocation
    /// ([`crate::authority::trigger_actor`]), whose change documents in the same edit what
    /// the trigger last did for the document. Each run is the workspace's once, on the device
    /// whose turn it is ([`ResourceStore::take_turn`]); a run the devices missed while all were
    /// off runs once when one comes up. Returns how many ran here.
    pub fn run_triggers(&self, now: i64) -> usize {
        crate::triggers::run_due(self, &self.inner.triggers, now)
    }

    /// The next instant strictly after `now` at which a trigger has something to run: what the
    /// host waits for, so nothing polls for it. `None` when no trigger will come due again.
    #[must_use]
    pub fn next_trigger(&self, now: i64) -> Option<i64> {
        crate::triggers::next_due(self, &self.inner.triggers, now)
    }

    /// Open the window of `app`'s root resource, through the default view of its type: its
    /// first root type with a default view, as the launcher opens an app.
    pub fn open_main(&self, app: &str) -> Result<ViewInstance, HostError> {
        self.open_layer(app, &RootView::default())
    }

    /// Open the window of the layer `chosen` names in `app`: the root it names and the view it
    /// names ([`RootView::default`] takes the package's first root and that type's default
    /// view).
    pub fn open_layer(&self, app: &str, chosen: &RootView) -> Result<ViewInstance, HostError> {
        self.open_layer_at(app, chosen).map(|(_, view)| view)
    }

    /// [`Self::open_layer`] with the resource it opened: the window's level keeps the root it
    /// shows, so what is opened is resolved once rather than once for the root and once for the
    /// view.
    ///
    /// A view that takes nothing (`view phone()`, a shell's own screen for a host with no desktop)
    /// opens over the root without taking it: the root is the window's, and a view says whether it
    /// is one of its arguments.
    pub fn open_layer_at(
        &self,
        app: &str,
        chosen: &RootView,
    ) -> Result<(ResourceRef, ViewInstance), HostError> {
        let package = self
            .package(app)
            .ok_or_else(|| HostError::NotFound(format!("package {app}")))?;
        let root = self.root_of_layer(app, chosen)?;
        let view = match (&chosen.view, &chosen.root) {
            (Some(name), _) => package.view_id(name),
            (None, Some(_)) => package.meta.default_view(&root.ty).map(str::to_owned),
            (None, None) => package.main().map(|(view, _)| view),
        };
        let view = view.ok_or_else(|| {
            HostError::NotFound(format!(
                "a view for {} in {app}",
                chosen.view.as_deref().unwrap_or("the root")
            ))
        })?;
        let takes_nothing = package
            .meta
            .views
            .iter()
            .find(|meta| meta.id == view)
            .is_some_and(|meta| meta.params.is_empty());
        let args = if takes_nothing {
            Vec::new()
        } else {
            vec![Value::resource(&root)]
        };
        let opened = self
            .inner
            .runtime
            .open_view(&self.inner.viewer, &view, args)?;
        Ok((root, opened))
    }

    /// Open the window of `resource`, as `open(r)` in a view asks: through `view`, or the default
    /// view of its type.
    pub fn open_resource(
        &self,
        resource: &ResourceRef,
        view: Option<&str>,
    ) -> Result<ViewInstance, HostError> {
        Ok(self
            .inner
            .runtime
            .open_resource(&self.inner.viewer, resource, view)?)
    }

    /// Plan an action as the API would, without an API.
    ///
    /// # Errors
    /// As [`Planner::plan`].
    pub fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        self.inner.plan(request)
    }

    /// The published documents of the workspace: each resource of a loaded package with its type name.
    #[must_use]
    pub fn records(&self) -> Vec<(Rc<Package>, String, String)> {
        let mut out = Vec::new();
        for package in self.packages() {
            for declared in &package.meta.types {
                for doc in self
                    .inner
                    .store
                    .resources_of(&declared.id)
                    .unwrap_or_default()
                {
                    out.push((Rc::clone(&package), declared.name.clone(), doc));
                }
            }
        }
        out
    }

    /// Whether `module` is trusted code.
    #[must_use]
    pub fn trusts(&self, module: &ModuleHash) -> bool {
        self.inner
            .packages
            .borrow()
            .iter()
            .any(|package| &package.id == module)
    }

    /// The label of a published root resource of the type `ty`; see [`Self::label_at`].
    #[must_use]
    pub fn label_of(&self, doc: &str, ty: &str) -> String {
        self.label_at(&ResourceRef::root(doc, ty.to_owned()))
    }

    /// The label of the resource `at`, as it is published: its first text, and while it has
    /// none, what its package calls one of its type (see [`crate::publish::untitled`]); never its
    /// id.
    #[must_use]
    pub fn label_at(&self, at: &ResourceRef) -> String {
        if let Some(text) = self.inner.store.first_text(at) {
            return text;
        }
        let package = self
            .inner
            .packages
            .borrow()
            .iter()
            .find(|package| {
                package
                    .meta
                    .types
                    .iter()
                    .any(|declared| declared.id == at.ty)
            })
            .cloned();
        match (package, lookup(&at.ty)) {
            (Some(package), Some(schema)) => {
                let words = self.runtime().words();
                crate::publish::untitled(&package.meta, schema, &*words)
            }
            (None, Some(schema)) => schema.spoken_name(),
            (_, None) => at.ty.clone(),
        }
    }

    /// The argument of the type `ty` (as metadata writes it) of an action of `meta` that
    /// `json` gives, read as the API reads an invocation's arguments: a resource by its id, a
    /// case by its name, a person as the workspace target they are.
    pub fn argument(&self, meta: &Meta, json: &Json, ty: &str) -> Result<Value, String> {
        value_of(&self.inner, meta, json, ty)
    }
}

/// The system time as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`: what `now` reads.
#[must_use]
pub fn system_time() -> String {
    let secs = tilo_clock::wall()
        .duration_since(tilo_clock::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    iso_utc(secs)
}

/// Seconds since the Unix epoch as ISO-8601 UTC text.
#[must_use]
pub fn iso_utc(secs: u64) -> String {
    let (h, m, s) = (secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let (year, month, day) =
        tilolang::calendar::civil_from_days(i64::try_from(secs / 86_400).unwrap_or(i64::MAX));
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}
