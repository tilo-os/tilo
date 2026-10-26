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
    ActionInfo, Decision, InvokeOutcome, ModuleHash, PackageId, Principal, Runtime, RuntimeError,
    StoreError, ViewInstance,
};

use crate::action::{Authorship, DynAction};
use crate::authority::TiloAuthority;
use crate::meta::MetaExt as _;
use crate::publish::{Caller, Committer, PlanRequest, Planner};
use crate::schema::lookup;
use crate::store::{Committed, TiloStore};

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

/// The answers a person gave agents, per app and kind of action.
type AgentRights = Rc<RefCell<HashMap<(String, tilolang_runtime::meta::EffectClass), Decision>>>;

pub(crate) struct HostInner {
    handle: u64,
    runtime: Runtime,
    store: TiloStore,
    authority: TiloAuthority,
    viewer: Principal,
    packages: RefCell<Vec<Package>>,
    /// Suggestions an agent's flow raised on the door, waiting for the window to ask a person.
    pub(crate) suspended: RefCell<std::collections::VecDeque<tilolang_runtime::Suggestion>>,
    /// The principals the door admitted as software acting for a person.
    pub(crate) agents: Rc<RefCell<std::collections::HashSet<String>>>,
    /// What the person narrowed an agent's rights to, per app and kind of action.
    rights: AgentRights,
    /// The resource the last action planned for the person at the window returned, with the
    /// action: what a window opens after its primary action made it.
    produced: RefCell<Option<(String, ResourceRef)>>,
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
}

pub(crate) fn value_of(
    host: &HostInner,
    meta: &Meta,
    json: &Json,
    ty: &str,
) -> Result<Value, String> {
    match parse_ty(ty, &meta.app)? {
        Ty::Record(key) => {
            // A parameter may be a type of another app of the room.
            let owner = if key.app == meta.app {
                meta.clone()
            } else {
                host.package_of(&key.app)?.meta
            };
            let declared = owner
                .type_named(&key.name)
                .ok_or_else(|| format!("unknown type `{}`", key.name))?;
            let text = json
                .as_str()
                .ok_or_else(|| format!("a {} is given by its resource id", key.name))?;
            let target = ResourceRef::parse_id(text, declared.id.clone())
                .ok_or_else(|| format!("`{text}` is not a resource id"))?;
            Ok(Value::resource(&target))
        }
        _ => from_json(json),
    }
}

impl HostInner {
    pub(crate) fn admit_agent(&self, actor: &str) {
        self.agents.borrow_mut().insert(actor.to_owned());
        self.authority.grants().trust_principal(actor);
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

    fn plan_in_mirror(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        let package = self.package_of(request.app)?;
        let action = package
            .meta
            .actions
            .iter()
            .find(|action| action.id == request.action_id)
            .ok_or_else(|| format!("no action {}", request.action_id))?;
        let mut args = Vec::new();
        let mut params = action.params.iter();
        if let (Some(subject), Some(declared)) =
            (request.resource, package.meta.subject_of(&action.params))
        {
            let target = ResourceRef::parse_id(subject, declared.id.clone())
                .ok_or_else(|| format!("`{subject}` is not a resource id"))?;
            args.push(Value::resource(&target));
            params.next();
        }
        for param in params {
            let json = request.args.get(&param.name).cloned().unwrap_or(Json::Null);
            args.push(value_of(self, &package.meta, &json, &param.ty)?);
        }
        // The runtime sees the invoker the door admitted: an agent's task delegate for itself,
        // the person at the window otherwise.
        let invoker = request.caller.map_or_else(
            || self.viewer.clone(),
            |caller| Principal::new(caller.actor.clone()),
        );
        if let Some(caller) = request.caller.filter(|caller| caller.agent) {
            self.admit_agent(&caller.actor);
        }
        let first = self.store.next_ticket();
        let outcome = self
            .runtime
            .invoke_action(&invoker, (request.action_id, args.clone()))
            .map_err(|error| error.to_string())?;
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
            args: Json::Object(request.args.clone()),
            authorship: Authorship {
                invoker: invoker.0,
                chain: Vec::new(),
                module: package.id.0.clone(),
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
    /// A host over the room record `record`, for `viewer`. `invoker` is the platform's verdict on
    /// what the viewer may do; code authorship is decided by the runtime's own rules.
    ///
    /// # Errors
    /// The runtime cannot be set up, which is a bug.
    pub fn new(
        record: &tilo_resource::Doc,
        viewer: Principal,
        invoker: impl Fn(&Principal, &ActionInfo) -> Decision + 'static,
    ) -> Result<Self, HostError> {
        Self::on_device(record, viewer, None, invoker)
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
        invoker: impl Fn(&Principal, &ActionInfo) -> Decision + 'static,
    ) -> Result<Self, HostError> {
        // The host's own record types (`Invocation`) are stored like any other resource.
        tilolang::host_meta().register_schemas();
        let store = device.map_or_else(
            || TiloStore::new(record),
            |device| TiloStore::on_device(record, device),
        );
        // An agent is a task of a person under that person's narrowed rights. Before anyone
        // chooses, reading and undoable changes are allowed to it and outside effects ask first;
        // the person narrows that per app and kind of action (`set_agent_answer`), and trusting
        // an agent ("always allow") lifts what asks.
        let agents: Rc<RefCell<std::collections::HashSet<String>>> = Rc::default();
        let rights: AgentRights = Rc::default();
        let (known, narrowed) = (Rc::clone(&agents), Rc::clone(&rights));
        let authority = TiloAuthority::new(move |who: &Principal, info: &ActionInfo| {
            let platform = invoker(who, info);
            if !known.borrow().contains(&who.0) {
                return platform;
            }
            let answer = narrowed
                .borrow()
                .get(&(info.app.clone(), info.class))
                .copied()
                .unwrap_or(
                    if info.class == tilolang_runtime::meta::EffectClass::Outside {
                        Decision::Ask
                    } else {
                        Decision::Allow
                    },
                );
            crate::authority::stricter(platform, answer)
        });
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
            agents,
            rights,
        });
        LIVE.with(|live| live.borrow_mut().insert(handle, Rc::downgrade(&inner)));
        Ok(Self { inner })
    }

    /// Replace what `now` reads (the system time until then): tests and captures fix it. The
    /// clock answers ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn set_clock(&self, clock: impl Fn() -> String + 'static) {
        self.inner.runtime.set_clock(clock);
    }

    /// The handle the door's closures hold.
    #[must_use]
    pub fn handle(&self) -> HostHandle {
        HostHandle {
            id: self.inner.handle,
            caller: None,
        }
    }

    /// Admit `actor` as software acting for a person: a task. Its code is trusted as far as its
    /// rights go ([`Self::set_agent_answer`]): what it may do, it does; what asks, asks a person.
    pub fn admit_agent(&self, actor: &str) {
        self.inner.admit_agent(actor);
    }

    /// Answer "always allow" for what `action` is: every action of its app and kind is allowed
    /// to agents from now on.
    pub fn always_allow_agents(&self, action: &str) {
        let found = self.inner.packages.borrow().iter().find_map(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|declared| declared.id == action)
                .map(|declared| (package.meta.app.clone(), declared.class))
        });
        if let Some((app, class)) = found {
            self.set_agent_answer(&app, class, Decision::Allow);
        }
    }

    /// Narrow (or widen) what agents may do: `answer` for each action of `class` of the app
    /// `app`. Before anyone chooses, reading and undoable changes are allowed and outside
    /// effects ask first.
    pub fn set_agent_answer(
        &self,
        app: &str,
        class: tilolang_runtime::meta::EffectClass,
        answer: Decision,
    ) {
        self.inner
            .rights
            .borrow_mut()
            .insert((app.to_owned(), class), answer);
    }

    /// The next suggestion an agent's flow raised, for the window to ask a person about.
    #[must_use]
    pub fn take_suspended(&self) -> Option<tilolang_runtime::Suggestion> {
        self.inner.suspended.borrow_mut().pop_front()
    }

    /// Carry out the outside effects of this device that wait in the record: each goes to the
    /// sink, and what settles is queued for the door like any other change. Returns how many
    /// were delivered.
    ///
    /// # Errors
    /// No sink is set, or the store failed.
    pub fn settle_outbox(&self) -> Result<usize, String> {
        if self.inner.store.outbox_pending_count() == 0 {
            return Ok(0);
        }
        self.inner
            .runtime
            .settle()
            .map(|report| report.delivered)
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

    /// Load the package an installed app is: its module, and the record schemas of the packages
    /// it uses, whose resources it reads but whose code it does not run.
    ///
    /// # Errors
    /// The module does not link.
    pub fn load_bundle(&self, bundle: &crate::bundle::Bundle) -> Result<Package, HostError> {
        for import in &bundle.imports {
            import.register_schemas();
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
        self.inner.authority.trust(&id);
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

    /// The resource kind and label of a published record.
    #[must_use]
    pub fn label_of(&self, doc: &str) -> String {
        let store = &self.inner.store;
        let ty = store.resource_type(doc).unwrap_or_default();
        store
            .first_text(&ResourceRef::root(doc, ty))
            .map_or_else(|| doc.to_owned(), |(text, _)| text)
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
/// A package that does not compile is left out and named in the log.
#[must_use]
pub fn builtin_surface(packages: &[(&str, &str)]) -> Vec<u8> {
    let doc = tilo_resource::Doc::new();
    let Ok(host) = Host::new(&doc, Principal::new("surface"), |_, _| Decision::Allow) else {
        return Vec::new();
    };
    for (source, ids) in packages {
        if let Err(error) = host.load_builtin(source, ids) {
            eprintln!("a built-in tilolang package did not compile: {error}");
        }
    }
    let metas: Vec<Meta> = host
        .packages()
        .into_iter()
        .map(|package| package.meta)
        .collect();
    let refs: Vec<&Meta> = metas.iter().collect();
    crate::surface::surface_toml(&refs).unwrap_or_default()
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
