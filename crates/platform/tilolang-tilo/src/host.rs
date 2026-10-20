//! [`Host`]: the tilolang runtime wired to the platform, without any UI.
//!
//! A host owns one [`Runtime`] over a [`TiloStore`] and the platform's [`TiloAuthority`]. It
//! compiles and loads packages, keeps their record schemas registered, opens a package's `main`
//! view over its per-room root resource, and plans door invocations into [`DynAction`]s. The
//! window widget in `apps/tilolang` drives it; so do the collaboration tests, with no window.
//!
//! The runtime is single-threaded. The closures the action door calls are `Send + Sync`, so they
//! hold a [`HostHandle`]: an id that finds its host in a thread-local registry, on the thread
//! that built it. A call from another thread is refused with a message.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
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
use crate::publish::{Committer, PlanRequest, Planner};
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
    /// The id of the view `main`, and the id of the type of its first parameter: the package's
    /// root resource.
    #[must_use]
    pub fn main(&self) -> Option<(String, String)> {
        let view = self.meta.views.iter().find(|view| view.name == "main")?;
        let param = view.params.first()?;
        let declared = self.meta.type_named(&param.ty)?;
        Some((view.id.clone(), declared.id.clone()))
    }
}

struct HostInner {
    handle: u64,
    runtime: Runtime,
    store: TiloStore,
    authority: TiloAuthority,
    viewer: Principal,
    packages: RefCell<Vec<Package>>,
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
    inner: Rc<HostInner>,
}

/// What the door's closures hold in place of the host: see the module docs.
#[derive(Clone, Copy, Debug)]
pub struct HostHandle {
    id: u64,
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

fn value_of(meta: &Meta, json: &Json, ty: &str) -> Result<Value, String> {
    match parse_ty(ty, &meta.app)? {
        Ty::Record(key) => {
            let declared = meta
                .type_named(&key.name)
                .filter(|_| key.app == meta.app)
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
    fn package_of(&self, app: &str) -> Result<Package, String> {
        self.packages
            .borrow()
            .iter()
            .find(|package| package.meta.app == app)
            .cloned()
            .ok_or_else(|| format!("no package {app}"))
    }

    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
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
            let target = ResourceRef::root(subject, declared.id.clone());
            args.push(Value::resource(&target));
            params.next();
        }
        for param in params {
            let json = request.args.get(&param.name).cloned().unwrap_or(Json::Null);
            args.push(value_of(&package.meta, &json, &param.ty)?);
        }
        let first = self.store.next_ticket();
        let outcome = self
            .runtime
            .invoke_action(&self.viewer, (request.action_id, args))
            .map_err(|error| error.to_string())?;
        let committed = self.store.take_from(first);
        match outcome {
            InvokeOutcome::Done(_) => {}
            InvokeOutcome::Suggested(_) => {
                return Err("the action needs approval first".to_owned());
            }
            InvokeOutcome::Denied => return Err("the action is not allowed".to_owned()),
        }
        let mut planned = DynAction {
            app: request.app.to_owned(),
            kind: request.kind.to_owned(),
            action_id: request.action_id.to_owned(),
            args: Json::Object(request.args.clone()),
            authorship: Authorship {
                invoker: self.viewer.0.clone(),
                chain: Vec::new(),
                module: package.id.0.clone(),
            },
            edits: Vec::new(),
        };
        for entry in committed {
            planned.edits.extend(entry.action.edits);
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
        let store = TiloStore::new(record);
        let authority = TiloAuthority::new(invoker);
        let runtime = Runtime::new(store.clone(), authority.clone())?;
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        let inner = Rc::new(HostInner {
            handle,
            runtime,
            store,
            authority,
            viewer,
            packages: RefCell::new(Vec::new()),
        });
        LIVE.with(|live| live.borrow_mut().insert(handle, Rc::downgrade(&inner)));
        Ok(Self { inner })
    }

    /// The handle the door's closures hold.
    #[must_use]
    pub fn handle(&self) -> HostHandle {
        HostHandle {
            id: self.inner.handle,
        }
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
        let meta = compiled.meta;
        meta.register_schemas();
        let id = self
            .inner
            .runtime
            .load_package(&compiled.wasm, meta.clone())?;
        self.inner.authority.trust(&id);
        let package = Package {
            meta,
            id,
            ids: compiled.ids.to_text(),
        };
        self.inner.packages.borrow_mut().push(package.clone());
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

    /// The actions the runtime committed that the host must commit through the executor.
    #[must_use]
    pub fn take_committed(&self) -> Vec<Committed> {
        self.inner.store.take_committed()
    }

    /// The package's root resource: the one per-room resource of the type its `main` view takes,
    /// created the first time it is wanted.
    ///
    /// # Errors
    /// The package has no `main` view over a record, or the resource cannot be created.
    pub fn root_of(&self, app: &str) -> Result<ResourceRef, HostError> {
        let package = self
            .package(app)
            .ok_or_else(|| HostError::NotFound(format!("package {app}")))?;
        let (_, ty) = package
            .main()
            .ok_or_else(|| HostError::NotFound(format!("a main view over a record in {app}")))?;
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

    /// Open the `main` view of `app` over its root resource.
    ///
    /// # Errors
    /// The package has no `main`, or the view does not run.
    pub fn open_main(&self, app: &str) -> Result<ViewInstance, HostError> {
        let package = self
            .package(app)
            .ok_or_else(|| HostError::NotFound(format!("package {app}")))?;
        let (view, _) = package
            .main()
            .ok_or_else(|| HostError::NotFound(format!("a main view in {app}")))?;
        let root = self.root_of(app)?;
        Ok(self
            .inner
            .runtime
            .open_view(&self.inner.viewer, &view, vec![Value::resource(&root)])?)
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
        self.inner
            .store
            .record(doc)
            .ok()
            .and_then(|record| {
                let mut text = Vec::new();
                tilo_resource::Model::collect_text(&record, &mut text);
                text.into_iter().next()
            })
            .unwrap_or_else(|| doc.to_owned())
    }

    /// Whether the type `ty` is registered (a package of it was loaded here).
    #[must_use]
    pub fn knows_type(ty: &str) -> bool {
        lookup(ty).is_some()
    }
}
