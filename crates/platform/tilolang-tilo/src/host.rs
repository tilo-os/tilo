//! [`Host`]: the tilolang runtime wired to the platform, without any UI.
//!
//! A host owns one [`Runtime`] over a [`TiloStore`] and the platform's [`TiloAuthority`]. It
//! compiles and loads packages, keeps their record schemas registered, opens a package's root
//! resource (`root T`, one per room) through the default view of its type, and plans door
//! invocations into [`DynAction`]s. The
//! window widget in `apps/tilolang` drives it; so do the collaboration tests, with no window.
//!
//! The runtime is single-threaded. The closures the action door calls are `Send + Sync`, so they
//! hold a [`HostHandle`]: an id that finds its host in a thread-local registry, on the thread
//! that built it. A call from another thread is refused with a message.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value as Json;
use tilo_actions::EncodedAction;
use tilolang::{IdsLock, PackageMeta};
use tilolang_runtime::meta::{Meta, Ty, parse_ty};
use tilolang_runtime::store::NewRecord;
use tilolang_runtime::value::{ResourceRef, Value, from_json};
use tilolang_runtime::{
    InvokeOutcome, ModuleHash, PackageId, Principal, Runtime, RuntimeError, StoreError,
    ViewInstance,
};

use crate::action::{Authorship, DynAction};
use crate::authority::TiloAuthority;
use crate::meta::MetaExt as _;
use crate::publish::{Caller, Committer, PlanRequest, Planner};
use crate::schema::lookup;
use crate::store::{Committed, TiloStore};
use tilo_action_policy::Standing;

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

/// A loaded package.
#[derive(Clone, Debug)]
pub struct Package {
    pub meta: Meta,
    pub id: PackageId,
    /// The ids lock after compiling, to write back next to the source.
    pub ids: String,
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
}

pub(crate) struct HostInner {
    handle: u64,
    runtime: Runtime,
    store: TiloStore,
    authority: TiloAuthority,
    viewer: Principal,
    packages: RefCell<Vec<Package>>,
    /// Suggestions an agent's flow raised on the door, waiting for the window to ask a person.
    pub(crate) suspended: RefCell<std::collections::VecDeque<tilolang_runtime::Suggestion>>,
    /// The resource the last action planned for the person at the window returned, with the
    /// action: what a window opens after its primary action made it.
    produced: RefCell<Option<(String, ResourceRef)>>,
    /// The resource the last action planned returned, whoever invoked it, with the action:
    /// what its invocation answers.
    returned: RefCell<Option<(String, ResourceRef)>>,
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

/// How a host opens: over which record, keeping its indexes where.
#[derive(Default)]
pub struct Opening<'a> {
    /// The room record; `None` to open over the indexes alone and take it later.
    pub record: Option<&'a tilo_resource::Doc>,
    /// Where each package's index is kept; `None` keeps them in memory.
    pub index_dir: Option<std::path::PathBuf>,
    /// The device that carries out the outside effects this host queues.
    pub device: Option<&'a str>,
}

/// What the door's closures hold in place of the host: see the module docs.
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

impl Committer for HostHandle {
    fn encoded(&self, ticket: u64) -> Result<EncodedAction, String> {
        with_host(self.id, |host| {
            let action = host
                .store
                .committed(ticket)
                .ok_or_else(|| format!("no committed action {ticket}"))?;
            action.encode().map_err(|error| error.to_string())
        })
    }

    fn need(&self, ticket: u64) -> Result<tilo_actions::ActionNeed, String> {
        with_host(self.id, |host| {
            host.store
                .committed(ticket)
                .map(|action| action.need())
                .ok_or_else(|| format!("no committed action {ticket}"))
        })
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

/// The value of the type `ty` an argument given as `json` stands for, as the door publishes the
/// parameter ([`crate::surface::published_param`]): a record by the catalogue's reference to it
/// (or its resource id), a plain enum by the name of its case, a principal as the workspace
/// subject it is, lists and options as plain JSON. A value encoded as the runtime stores it
/// ([`from_json`]) is taken as it is.
fn value_of_ty(host: &HostInner, meta: &Meta, json: &Json, ty: &Ty) -> Result<Value, String> {
    match (ty, json) {
        (Ty::Record(key), _) => {
            // A parameter may be a type of another app of the room.
            let owner = if key.app == meta.app {
                meta.clone()
            } else {
                host.package_of(&key.app)?.meta
            };
            let declared = owner
                .type_named(&key.name)
                .ok_or_else(|| format!("unknown type `{}`", key.name))?;
            let text = match json {
                Json::String(text) => text.as_str(),
                Json::Object(reference) => {
                    let kind = owner.kind_of(&declared.name);
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
            let target = ResourceRef::parse_id(id, declared.id.clone())
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
        (Ty::Principal, Json::Object(subject)) if subject.len() == 1 => {
            let principal = match (subject.get("person"), subject.get("agent")) {
                (Some(Json::String(person)), None) => person.clone(),
                (None, Some(Json::String(agent))) => format!("agent:{agent}"),
                _ => from_json(json).and_then(|value| match value {
                    Value::Principal(principal) => Ok(principal.0),
                    _ => Err("a principal is a person or an agent".to_owned()),
                })?,
            };
            Ok(Value::Principal(Principal::new(principal)))
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
        // A union of records: the reference's kind says which of them it is.
        (Ty::Union(keys), _) => union_value(host, meta, json, keys),
        _ => from_json(json),
    }
}

/// The record of one of the types `keys` that the reference `json` names by its kind.
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
        .find(|key| Some(crate::surface::record_kind(key).as_str()) == kind)
        .ok_or_else(|| {
            format!(
                "a {} is a resource of kind {}, not {}",
                union_label(keys),
                keys.iter()
                    .map(crate::surface::record_kind)
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

/// The argument `value` of the type `ty`, as the door takes it: in the shape the action's
/// published parameter declares ([`crate::surface::published_param`]), which the broker binds
/// before it delivers, and which [`value_of`] reads back as the same value. A record by the
/// catalogue's reference to it, of its own type, so a union or a type of another app says which
/// it is; a plain enum by its case's name; a principal as the workspace subject it is; an option
/// as its value, or null; lists and tuples item by item; rich text as its text, or encoded
/// whole when it holds more than text. `owner` finds the package of an app the type names.
///
/// # Errors
/// A value that is not of `ty`, or names a type no package declares.
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
                "kind": crate::surface::record_kind(key),
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
        // A flag travels as the integer the guest boxed it as.
        (Ty::Bool, Value::Int(n)) => Ok(Json::Bool(*n != 0)),
        // A plain enum's parameter chooses among its cases by name
        // ([`crate::surface::published_param`]); one with payloads takes the value encoded.
        (Ty::Enum(key), Value::Enum(chosen)) if key.app == meta.app => {
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
                Some(_) => to_json(value),
                None => Err(format!(
                    "`{}` is not a case of {}",
                    chosen.variant, key.name
                )),
            }
        }
        (Ty::Principal, Value::Principal(principal)) => {
            Ok(match principal.0.strip_prefix("agent:") {
                Some(agent) => serde_json::json!({ "agent": agent }),
                None => serde_json::json!({ "person": principal.0 }),
            })
        }
        (Ty::RichText, Value::Rich(rich)) => {
            let text = rich.written_text();
            if tilolang_runtime::rich::RichValue::from_text(&text) == *rich {
                Ok(Json::String(text))
            } else {
                to_json(value)
            }
        }
        (Ty::Record(_) | Ty::Union(_), other) => Err(format!(
            "a resource is given by a reference to it, not {}",
            to_json(other).unwrap_or(Json::Null)
        )),
        _ => to_json(value),
    }
}

impl HostInner {
    pub(crate) fn admit_agent(&self, caller: &Caller) {
        self.authority
            .admit_agent(&caller.actor, caller.on_behalf_of.as_deref());
    }

    pub(crate) fn package_of(&self, app: &str) -> Result<Package, String> {
        self.packages
            .borrow()
            .iter()
            .find(|package| package.meta.app == app)
            .cloned()
            .ok_or_else(|| format!("no package {app}"))
    }

    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        // Planning is a trial run: whatever it wrote into the mirror is taken back, so asking
        // for an action (to describe it, to offer it) never changes the room.
        let checkpoint = self.store.checkpoint();
        let planned = self.plan_in_mirror(request);
        self.store.rewind(checkpoint);
        planned
    }

    /// The argument `value` of `param` as an invocation records it: as the door takes it, so
    /// the recorded invocation reads back as it ran ([`argument_json`]).
    fn recorded(
        &self,
        meta: &Meta,
        param: &tilolang::meta::ParamMeta,
        value: &Value,
    ) -> Result<Json, String> {
        argument_json(
            &|app| self.package_of(app).map(|package| package.meta),
            meta,
            value,
            &parse_ty(&param.ty, &meta.app)?,
        )
    }

    /// What `action` returned, for its invocation's answer.
    fn returned_by(&self, action: &str, outcome: &InvokeOutcome) {
        *self.returned.borrow_mut() = match outcome {
            InvokeOutcome::Done(Value::Ref(made)) => {
                Some((action.to_owned(), ResourceRef::clone(&made.target)))
            }
            _ => None,
        };
    }

    fn plan_in_mirror(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        let package = self.package_of(request.app)?;
        let action = package
            .meta
            .actions
            .iter()
            .find(|action| action.id == request.action_id)
            .ok_or_else(|| format!("no action {}", request.action_id))?;
        let mut given: Vec<Option<Value>> = Vec::new();
        let mut params = action.params.iter();
        if let (Some(subject), Some(declared)) =
            (request.resource, package.meta.subject_of(&action.params))
        {
            let target = ResourceRef::parse_id(subject, declared.id.clone())
                .ok_or_else(|| format!("`{subject}` is not a resource id"))?;
            given.push(Some(Value::resource(&target)));
            params.next();
        }
        // An argument left out that has a default gets it now, on the invoker's state: what
        // the person saw is the argument, and the invocation records it.
        let mut defaulted = Vec::new();
        for param in params {
            match request.args.get(&param.name) {
                None if param.default.is_some() => {
                    defaulted.push(param);
                    given.push(None);
                }
                json => {
                    let json = json.cloned().unwrap_or(Json::Null);
                    given.push(Some(value_of(self, &package.meta, &json, &param.ty)?));
                }
            }
        }
        let args = self
            .runtime
            .complete_args(request.action_id, given)
            .map_err(|error| error.to_string())?;
        let mut recorded = request.args.clone();
        for param in defaulted {
            let index = action
                .params
                .iter()
                .position(|declared| declared.name == param.name);
            if let Some(value) = index.and_then(|index| args.get(index)) {
                let json = self.recorded(&package.meta, param, value)?;
                recorded.insert(param.name.clone(), json);
            }
        }
        // The runtime sees the invoker the door admitted: an agent's task delegate for itself,
        // the person at the window otherwise.
        let invoker = request.caller.map_or_else(
            || self.viewer.clone(),
            |caller| Principal::new(caller.actor.clone()),
        );
        if let Some(caller) = request.caller.filter(|caller| caller.agent) {
            self.admit_agent(caller);
        }
        let first = self.store.next_ticket();
        let outcome = self
            .runtime
            .invoke_action(&invoker, (request.action_id, args.clone()))
            .map_err(|error| error.to_string())?;
        self.returned_by(request.action_id, &outcome);
        match outcome {
            InvokeOutcome::Done(Value::Ref(made))
                if request.caller.is_none_or(|caller| !caller.agent) =>
            {
                *self.produced.borrow_mut() = Some((
                    request.action_id.to_owned(),
                    std::rc::Rc::unwrap_or_clone(made.target),
                ));
            }
            InvokeOutcome::Done(_) => {}
            // An action that needs a person's approval is not refused: it is recorded as a
            // suggested invocation, the answer an agent gets and a person decides.
            InvokeOutcome::Suggested(suggestion) => {
                self.runtime
                    .record_suggestion(&invoker, request.action_id, args, &suggestion)
                    .map_err(|error| error.to_string())?;
            }
            InvokeOutcome::Denied => return Err("the action is not allowed".to_owned()),
        }
        let committed = self.store.take_from(first);
        let mut planned = DynAction {
            app: request.app.to_owned(),
            kind: request.kind.to_owned(),
            action_id: request.action_id.to_owned(),
            args: Json::Object(recorded),
            authorship: Authorship {
                invoker: invoker.0,
                chain: Vec::new(),
                module: package.id.0.clone(),
                at: None,
            },
            edits: Vec::new(),
        };
        for entry in committed {
            // Follow-up changes of the host itself (an action block's invocation record) belong
            // to another app's resources than the action's.
            let other = entry.action.app != planned.app;
            for edit in entry.action.edits {
                planned.edits.push(if other {
                    edit.in_app(&entry.action.app)
                } else {
                    edit
                });
            }
        }
        Ok(planned)
    }
}

impl Host {
    /// A host over the room record `record`, for `viewer`, whose standing in the room `standing`
    /// reads: what the runtime invokes is decided by the room's policy ([`TiloAuthority`]).
    ///
    /// # Errors
    /// The runtime cannot be set up, which is a bug.
    pub fn new(
        record: &tilo_resource::Doc,
        viewer: Principal,
        standing: impl Fn() -> Option<Standing> + 'static,
    ) -> Result<Self, HostError> {
        Self::on_device(record, viewer, None, standing)
    }

    /// As [`Self::new`], on the device `device`: the device that carries out the outside effects
    /// this host queues. A host without one is a device of its own.
    ///
    /// # Errors
    /// The runtime cannot be set up, which is a bug.
    pub fn on_device(
        record: &tilo_resource::Doc,
        viewer: Principal,
        device: Option<&str>,
        standing: impl Fn() -> Option<Standing> + 'static,
    ) -> Result<Self, HostError> {
        Self::open(
            Opening {
                record: Some(record),
                index_dir: None,
                device,
            },
            viewer,
            standing,
        )
    }

    /// A host as `opening` says: over the room record, or (without it) over the indexes kept
    /// in its index directory alone, taking the record when it is had (see
    /// [`TiloStore::detached`]).
    ///
    /// # Errors
    /// The runtime cannot be set up, which is a bug.
    pub fn open(
        opening: Opening<'_>,
        viewer: Principal,
        standing: impl Fn() -> Option<Standing> + 'static,
    ) -> Result<Self, HostError> {
        let store = match (opening.record, opening.index_dir) {
            (Some(record), dir) => {
                let store = TiloStore::new(record);
                if let Some(dir) = dir {
                    store.set_index_dir(dir);
                }
                store
            }
            (None, Some(dir)) => TiloStore::detached(dir),
            (None, None) => TiloStore::new(&tilo_resource::Doc::new()),
        };
        if let Some(device) = opening.device {
            store.set_device(device);
        }
        // The host's own record types (`Invocation`) are stored like any other resource.
        let host_meta = tilolang::host_meta();
        host_meta.register_schemas();
        store.open_index(&host_meta.app, &host_meta.schema_text());
        // What the runtime invokes is decided by the room's one policy: the person's standing,
        // an agent's delegation and the answers the person gave it, and the code's authors.
        let authority = TiloAuthority::new(viewer.clone(), standing);
        let runtime = Runtime::new(store.clone(), authority.clone())?;
        runtime.set_clock(system_time);
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        let inner = Rc::new(HostInner {
            handle,
            runtime,
            store,
            authority,
            viewer,
            packages: RefCell::new(Vec::new()),
            suspended: RefCell::default(),
            produced: RefCell::default(),
            returned: RefCell::default(),
        });
        LIVE.with(|live| live.borrow_mut().insert(handle, Rc::downgrade(&inner)));
        Ok(Self { inner })
    }

    /// Whether every loaded package's index is read from: until then its lists show nothing,
    /// and the window shows the platform's loading state (see [`TiloStore::index_ready`]).
    #[must_use]
    pub fn index_ready(&self) -> bool {
        self.inner.store.index_ready()
    }

    /// Take in indexes built off the frame; whether one was (see [`TiloStore::poll_index`]).
    pub fn poll_index(&self) -> bool {
        self.inner.store.poll_index()
    }

    /// Wait for the indexes being built and take them in: for a host without frames.
    pub fn wait_index(&self) -> bool {
        self.inner.store.wait_index()
    }

    /// Have `wake` called when an index built off the frame is ready to be taken in.
    pub fn set_index_waker(&self, wake: crate::background::Waker) {
        self.inner.store.set_waker(wake);
    }

    /// Wait for what the indexes hold to reach the disk: when the window closes.
    pub fn persist_index(&self) {
        self.inner.store.persist_index();
    }

    /// How the host gets the room record when it opened without it: for a write, or a field
    /// no index holds.
    pub fn set_record_loader(&self, loader: crate::store::Loader) {
        self.inner.store.set_loader(loader);
    }

    /// Replace what `now` reads (the system time until then): tests and captures fix it. The
    /// clock answers ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn set_clock(&self, clock: impl Fn() -> String + 'static) {
        self.inner.runtime.set_clock(clock);
    }

    /// What the platform's phrases say in the person's language (`phrase(...)`, and what a
    /// reference to a resource they do not hold says wherever rich text is read for them).
    pub fn set_phrases(&self, phrases: impl Fn(&str, &[(String, String)]) -> String + 'static) {
        self.inner.runtime.set_phrases(phrases);
        self.inner
            .store
            .set_unshared(&self.inner.runtime.unshared());
    }

    /// The handle the door's closures hold.
    #[must_use]
    pub fn handle(&self) -> HostHandle {
        HostHandle {
            id: self.inner.handle,
            caller: None,
        }
    }

    /// Admit `caller` as software acting for a person: a task. It acts with that person's
    /// authority bounded by its delegation and the answers the person gave it in the room; its
    /// code is trusted as far as that goes.
    pub fn admit_agent(&self, caller: &Caller) {
        self.inner.admit_agent(caller);
    }

    /// The next suggestion an agent's flow raised, for the window to ask a person about.
    #[must_use]
    pub fn take_suspended(&self) -> Option<tilolang_runtime::Suggestion> {
        self.inner.suspended.borrow_mut().pop_front()
    }

    /// Carry out the outside effects of this device that wait in the record
    /// ([`Runtime::settle`]): each is marked as being handed over, then goes to the sink, and
    /// what became of it is queued for the door like any other change. Returns how many
    /// changes that wrote. What `delivery(r)` reads follows the record either way.
    ///
    /// # Errors
    /// No sink is set, or the store failed.
    pub fn settle_outbox(&self) -> Result<usize, String> {
        if self.inner.store.outbox_pending_count() == 0 {
            self.inner.runtime.refresh_deliveries();
            return Ok(0);
        }
        self.inner
            .runtime
            .settle()
            .map(|report| {
                [
                    report.failed.len(),
                    report.marked,
                    report.waiting,
                    report.unknown,
                ]
                .into_iter()
                .fold(report.delivered, usize::saturating_add)
            })
            .map_err(|error| error.to_string())
    }

    /// The runtime, for flows, blocks and the catalogue.
    #[must_use]
    pub fn runtime(&self) -> &Runtime {
        &self.inner.runtime
    }

    #[must_use]
    pub fn store(&self) -> &TiloStore {
        &self.inner.store
    }

    #[must_use]
    pub fn authority(&self) -> &TiloAuthority {
        &self.inner.authority
    }

    /// The resource `action` returned when the person last ran it here, once: a window opens
    /// what its primary action made.
    #[must_use]
    pub fn take_produced(&self, action: &str) -> Option<ResourceRef> {
        let mut produced = self.inner.produced.borrow_mut();
        match produced.as_ref() {
            Some((made_by, _)) if made_by == action => produced.take().map(|(_, made)| made),
            _ => None,
        }
    }

    /// The resource `action` returned when it was last planned here, for whoever invoked it,
    /// once: what its invocation answers.
    #[must_use]
    pub fn take_returned(&self, action: &str) -> Option<ResourceRef> {
        let mut returned = self.inner.returned.borrow_mut();
        match returned.as_ref() {
            Some((made_by, _)) if made_by == action => returned.take().map(|(_, made)| made),
            _ => None,
        }
    }

    #[must_use]
    pub fn viewer(&self) -> &Principal {
        &self.inner.viewer
    }

    /// Compile `source` against the packages already loaded, register its schemas, load its
    /// module and trust it (a built-in package).
    ///
    /// # Errors
    /// The source does not compile, or the module does not link.
    pub fn load_builtin(&self, source: &str, ids: &str) -> Result<Package, HostError> {
        let lock = IdsLock::parse(ids).map_err(|error| HostError::Compile(error.to_string()))?;
        let imports: Vec<PackageMeta> = self
            .inner
            .packages
            .borrow()
            .iter()
            .map(|package| package.meta.clone())
            .collect();
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
        self.load_compiled(&compiled)
    }

    /// Load a compiled package and trust it (a built-in package, or the package being developed).
    /// A package of the same app that is already loaded is replaced: its resources keep their
    /// state, and views opened over the old version must be opened again to run the new code.
    ///
    /// # Errors
    /// The module does not link; the loaded version, if any, stays.
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
    ///
    /// # Errors
    /// A module does not link.
    pub fn load_bundle(&self, bundle: &crate::bundle::Bundle) -> Result<Package, HostError> {
        for import in &bundle.imports {
            self.load_module(&import.wasm, import.meta.clone(), String::new())?;
        }
        self.load_module(&bundle.wasm, bundle.meta.clone(), bundle.ids.clone())
    }

    /// Load a package from its compiled module alone, with the metadata the module embeds, and
    /// trust it: how a launcher starts a built-in package compiled when Tilo was built, so that
    /// launching compiles no tilolang. The package has no ids lock to write back.
    ///
    /// # Errors
    /// The module embeds no metadata, or does not load.
    pub fn load_wasm(&self, wasm: &[u8]) -> Result<Package, HostError> {
        let meta = PackageMeta::from_wasm(wasm).ok_or_else(|| {
            HostError::Compile("the module embeds no tilolang metadata".to_owned())
        })?;
        self.load_module(wasm, meta, String::new())
    }

    /// Load a compiled module with its metadata and trust it; see [`Self::load_compiled`].
    ///
    /// # Errors
    /// The module does not link; the loaded version, if any, stays.
    pub fn load_module(&self, wasm: &[u8], meta: Meta, ids: String) -> Result<Package, HostError> {
        let id = self.inner.runtime.reload_package(wasm, meta.clone())?;
        meta.register_schemas();
        self.inner.store.open_index(&meta.app, &meta.schema_text());
        self.inner.authority.load(&id, &meta);
        let package = Package { meta, id, ids };
        let mut packages = self.inner.packages.borrow_mut();
        match packages
            .iter_mut()
            .find(|loaded| loaded.meta.app == package.meta.app)
        {
            Some(loaded) => *loaded = package.clone(),
            None => packages.push(package.clone()),
        }
        Ok(package)
    }

    /// Every loaded package.
    #[must_use]
    pub fn packages(&self) -> Vec<Package> {
        self.inner.packages.borrow().clone()
    }

    /// The loaded package `app`.
    #[must_use]
    pub fn package(&self, app: &str) -> Option<Package> {
        self.inner.package_of(app).ok()
    }

    /// Bring the runtime's view of the room record up to date; see [`TiloStore::sync_from`].
    pub fn sync_from(&self, record: &tilo_resource::Doc) -> bool {
        self.inner.store.sync_from(record)
    }

    /// Whether the runtime reads exactly what the room record `record` holds; see
    /// [`TiloStore::is_at`].
    #[must_use]
    pub fn is_at(&self, record: &tilo_resource::Doc) -> bool {
        self.inner.store.is_at(record)
    }

    /// The actions the runtime committed under a ticket of `first` or later.
    #[must_use]
    pub fn take_committed_from(&self, first: u64) -> Vec<Committed> {
        self.inner.store.take_committed_from(first)
    }

    /// The ticket the next committed action will be queued under.
    #[must_use]
    pub fn next_ticket(&self) -> u64 {
        self.inner.store.next_ticket()
    }

    /// The actions the runtime committed that the host must commit through the executor.
    #[must_use]
    pub fn take_committed(&self) -> Vec<Committed> {
        self.inner.store.take_committed()
    }

    /// The package's root resource: the one per-room resource of its root type, created the
    /// first time it is wanted.
    ///
    /// # Errors
    /// The package declares no root with a default view, or the resource cannot be created.
    pub fn root_of(&self, app: &str) -> Result<ResourceRef, HostError> {
        let package = self
            .package(app)
            .ok_or_else(|| HostError::NotFound(format!("package {app}")))?;
        let (_, ty) = package.main().ok_or_else(|| {
            HostError::NotFound(format!("a root type with a default view in {app}"))
        })?;
        let id = format!("root-{ty}");
        if self.inner.store.contains(&id) {
            return Ok(ResourceRef::root(id, ty));
        }
        let record = NewRecord {
            ty,
            app: app.to_owned(),
            fields: Vec::new(),
        };
        Ok(self.inner.store.create_with_id(&record, &id)?)
    }

    /// Run each loaded package's migrations (`migration action`) on the resources an earlier
    /// version of it wrote: once on each resource of the migration's subject type that holds
    /// something in a field the type now keeps `(previously)` (or, for a type without such a
    /// field, that the migration has not run on), as an action of the package version
    /// ([`crate::authority::migration_principal`]), which records in the same change that it
    /// ran. Only the room's origin calls it (`Instance::set_origin`). Returns how many ran. A migration that is
    /// refused or fails leaves its resource as it was, to be tried again the next time.
    pub fn migrate(&self) -> usize {
        let mut ran: usize = 0;
        for package in self.packages() {
            for migration in package.meta.migrations() {
                let Some(ty) = migration
                    .params
                    .first()
                    .and_then(|subject| package.meta.type_named(&subject.ty))
                else {
                    continue;
                };
                let previously: Vec<&str> = ty
                    .fields
                    .iter()
                    .filter(|f| f.role.as_deref() == Some("previously"))
                    .map(|f| f.id.as_str())
                    .collect();
                for doc in self.inner.store.resources_of(&package.meta.app, &ty.id) {
                    if self
                        .inner
                        .store
                        .migrated(&doc, &migration.id)
                        .unwrap_or(true)
                        || !self.holds_previously(&doc, &previously)
                    {
                        continue;
                    }
                    let subject = Value::resource(&ResourceRef::root(&doc, ty.id.clone()));
                    self.inner
                        .store
                        .set_migrating(Some((migration.id.clone(), doc.clone())));
                    // The package updating its own records, not the person at the window: the
                    // change is the package version's, under the platform's migration principal.
                    let by = crate::authority::migration_principal(
                        &package.meta.app,
                        &package.meta.module_hash,
                    );
                    let outcome = self
                        .inner
                        .runtime
                        .invoke_action(&by, (migration.id.as_str(), vec![subject]));
                    self.inner.store.set_migrating(None);
                    match outcome {
                        Ok(InvokeOutcome::Done(_)) => ran = ran.saturating_add(1),
                        other => tracing::warn!(
                            app = package.meta.app,
                            migration = migration.name,
                            doc,
                            ?other,
                            "a migration did not run"
                        ),
                    }
                }
            }
        }
        ran
    }

    /// Whether the resource `doc` holds anything in the fields `previously` (none: whether it
    /// may hold what an earlier version wrote, which only the migration can tell).
    fn holds_previously(&self, doc: &str, previously: &[&str]) -> bool {
        if previously.is_empty() {
            return true;
        }
        let Ok(record) = self.inner.store.record(doc) else {
            return false;
        };
        let Ok(empty) = crate::record::DynRecord::empty(record.schema()) else {
            return false;
        };
        previously
            .iter()
            .any(|field| record.get(field) != empty.get(field))
    }

    /// Open the window of `app`'s root resource, through the default view of its type.
    ///
    /// # Errors
    /// The package has no root with a default view, or the view does not run.
    pub fn open_main(&self, app: &str) -> Result<ViewInstance, HostError> {
        let package = self
            .package(app)
            .ok_or_else(|| HostError::NotFound(format!("package {app}")))?;
        let (view, _) = package.main().ok_or_else(|| {
            HostError::NotFound(format!("a root type with a default view in {app}"))
        })?;
        let root = self.root_of(app)?;
        Ok(self
            .inner
            .runtime
            .open_view(&self.inner.viewer, &view, vec![Value::resource(&root)])?)
    }

    /// Open the window of `resource`, as `open(r)` in a view asks: through `view`, or the default
    /// view of its type.
    ///
    /// # Errors
    /// The type has no default view and none is given, or the view does not run.
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

    /// Plan an action as the door would, without a door.
    ///
    /// # Errors
    /// As [`Planner::plan`].
    pub fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        self.inner.plan(request)
    }

    /// The published records of the room: each resource of a loaded package with its type name.
    #[must_use]
    pub fn records(&self) -> Vec<(Package, String, String)> {
        let mut out = Vec::new();
        for package in self.packages() {
            for declared in &package.meta.types {
                for doc in self
                    .inner
                    .store
                    .resources_of(&package.meta.app, &declared.id)
                {
                    out.push((package.clone(), declared.name.clone(), doc));
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

    /// The label of a published record; see [`Self::label_at`].
    #[must_use]
    pub fn label_of(&self, doc: &str) -> String {
        let ty = self.inner.store.resource_type(doc).unwrap_or_default();
        self.label_at(&ResourceRef::root(doc, ty))
    }

    /// The label of the resource `at`, as it is published: its first text, and while it has
    /// none, what its package calls one of its type (see [`crate::publish::untitled`]); never its
    /// id.
    #[must_use]
    pub fn label_at(&self, at: &ResourceRef) -> String {
        if let Some((text, _)) = self.inner.store.first_text(at) {
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
            (Some(package), Some(schema)) => crate::publish::untitled(&package.meta, schema),
            (None, Some(schema)) => schema.spoken_name(),
            (_, None) => at.ty.clone(),
        }
    }

    /// The argument of the type `ty` (as metadata writes it) of an action of `meta` that
    /// `json` gives, read as the door reads an invocation's arguments: a resource by its id, a
    /// case by its name, a person as the workspace subject they are.
    ///
    /// # Errors
    /// `json` does not read as a value of `ty`, with why.
    pub fn argument(&self, meta: &Meta, json: &Json, ty: &str) -> Result<Value, String> {
        value_of(&self.inner, meta, json, ty)
    }

    /// Whether the type `ty` is registered (a package of it was loaded here).
    #[must_use]
    pub fn knows_type(ty: &str) -> bool {
        lookup(ty).is_some()
    }
}

/// The `surface.toml` bytes of the host with these built-in packages (source and ids lock),
/// generated from their metadata: what receivers decide the host's changes against.
///
/// # Errors
/// A package does not compile, or the surface cannot be written: never a surface missing a
/// package receivers would then decide its changes without.
pub fn builtin_surface(packages: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let doc = tilo_resource::Doc::new();
    let host = Host::new(&doc, Principal::new("surface"), || None)
        .map_err(|error| format!("no runtime to load the built-in packages: {error}"))?;
    for (source, ids) in packages {
        host.load_builtin(source, ids)
            .map_err(|error| format!("a built-in tilolang package did not compile: {error}"))?;
    }
    let metas: Vec<Meta> = host
        .packages()
        .into_iter()
        .map(|package| package.meta)
        .collect();
    let refs: Vec<&Meta> = metas.iter().collect();
    crate::surface::surface_toml(&refs).map_err(|error| error.to_string())
}

/// The system time as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`: what `now` reads.
#[must_use]
pub fn system_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    iso_utc(secs)
}

/// Seconds since the Unix epoch as ISO-8601 UTC text.
#[must_use]
pub fn iso_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rest = secs % 86_400;
    let (h, m, s) = (rest / 3600, rest % 3600 / 60, rest % 60);
    // Days to a civil date (proleptic Gregorian), after Howard Hinnant's `civil_from_days`.
    // Every step stays far inside `u64` for any time a clock reports.
    let z = days.wrapping_add(719_468);
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = doe
        .wrapping_sub(doe / 1460)
        .wrapping_add(doe / 36_524)
        .wrapping_sub(doe / 146_096)
        / 365;
    let doy = doe.wrapping_sub(
        yoe.wrapping_mul(365)
            .wrapping_add(yoe / 4)
            .wrapping_sub(yoe / 100),
    );
    let mp = doy.wrapping_mul(5).wrapping_add(2) / 153;
    let day = doy
        .wrapping_sub(mp.wrapping_mul(153).wrapping_add(2) / 5)
        .wrapping_add(1);
    let month = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    let year = yoe
        .wrapping_add(era.wrapping_mul(400))
        .wrapping_add(u64::from(month <= 2));
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}
