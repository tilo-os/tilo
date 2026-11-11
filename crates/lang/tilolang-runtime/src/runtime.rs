//! The public face of the runtime.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value as Json;

use crate::authority::Authority;
use crate::catalogue::{Signature, catalogue};
use crate::core::{Core, ModelHost, OutsideSink};
use crate::engine::EngineLimits;
use crate::error::RuntimeError;
use crate::exec::InvokeOutcome;
use crate::ids::{DocId, ModuleHash, PackageId, Principal, RunId};
use crate::meta::Meta;
use crate::project::{ProjectOptions, project};
use crate::run::Resolution;
use crate::settle::{AgentBlock, InvocationRecord};
use crate::store::{ChangeNote, OutboxEntry, Store, StoreError, Version};
use crate::value::{ActionRef, ResourceRef, Value};
use crate::view_instance::ViewInstance;
use tilolang::{BlockKind, Compiled, CompiledBlock};

/// What settling the outbox did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettleReport {
    pub delivered: usize,
    /// Entries the service refused, with its reason. They are settled as rejected.
    pub failed: Vec<(String, String)>,
    /// Entries marked as being handed over: handed over on the next settle, once the mark is
    /// in the record.
    pub marked: usize,
    /// Entries whose service could not be reached: they wait to go online.
    pub waiting: usize,
    /// Entries another process was handing over when it stopped: outcome unknown.
    pub unknown: usize,
    /// Entries held back until a moment that has not come yet: nothing was done with them.
    pub held: usize,
}

/// This process's token, which an outbox entry it marks as being handed over carries: an
/// entry marked by another token was being handed over by a process that stopped (the record
/// has one writer on a device).
fn process_token() -> &'static str {
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        format!("{}-{nanos:x}", std::process::id())
    })
}

/// What evaluating a block produced.
pub enum BlockOutput {
    View(Box<ViewInstance>),
    Action(InvokeOutcome),
    Data(Value),
}

/// An in-memory sink for tests and demos. Clones share the delivered list.
#[derive(Clone, Default)]
pub struct MemorySink {
    delivered: Rc<RefCell<Vec<(String, Json)>>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything delivered so far: capability and payload.
    pub fn delivered(&self) -> Vec<(String, Json)> {
        self.delivered.borrow().clone()
    }
}

impl OutsideSink for MemorySink {
    fn deliver(&mut self, capability: &str, payload: &Json) -> crate::core::Delivery {
        self.delivered
            .borrow_mut()
            .push((capability.to_owned(), payload.clone()));
        crate::core::Delivery::Delivered
    }
}

/// The tilolang runtime: loads packages and runs their actions, programs and views.
pub struct Runtime {
    pub(crate) core: Rc<Core>,
}

impl Runtime {
    /// A runtime over `store`, deciding invocations with `authority`.
    ///
    /// # Errors
    ///
    /// A message when the engine cannot be set up, which is a bug.
    pub fn new(
        store: impl Store + 'static,
        authority: impl Authority + 'static,
    ) -> Result<Self, RuntimeError> {
        Self::with_limits(store, authority, EngineLimits::default())
    }

    /// As [`Runtime::new`], with explicit fuel and memory limits.
    ///
    /// # Errors
    ///
    /// As [`Runtime::new`].
    pub fn with_limits(
        store: impl Store + 'static,
        authority: impl Authority + 'static,
        limits: EngineLimits,
    ) -> Result<Self, RuntimeError> {
        let core = Core::new(Box::new(store), Box::new(authority), limits)
            .map_err(RuntimeError::Package)?;
        let runtime = Self { core };
        // The types the host provides to every module, such as `Invocation`.
        runtime.publish_types();
        Ok(runtime)
    }

    /// Compile a module (cached by content hash), check it exports everything its metadata
    /// declares, and register it. Loading the same module twice returns the same id.
    ///
    /// # Errors
    ///
    /// `Package` for a module that does not link, lacks a declared export, or reuses another
    /// package's declaration ids.
    pub fn load_package(&self, wasm: &[u8], meta: Meta) -> Result<PackageId, RuntimeError> {
        let hash = self.core.engine.compile(wasm)?;
        self.add_catalogues(wasm, crate::words::WordsRole::Window);
        self.check_exports(&hash, &meta)?;
        let lines = tilolang::meta::lines_from_wasm(wasm);
        let pkg = crate::registry::Package::with_lines(hash.clone(), meta, &lines)
            .map_err(RuntimeError::Package)?;
        self.core
            .registry
            .borrow_mut()
            .add(pkg)
            .map_err(RuntimeError::Package)?;
        self.publish_types();
        Ok(hash)
    }

    /// Load a new version of a package, replacing every loaded version of the same app: what a
    /// development reload does after the sources changed. Resources keep their state, since they
    /// are stored by declaration id; views opened over the old version keep running it until
    /// they are opened again.
    ///
    /// # Errors
    ///
    /// As [`Runtime::load_package`]; the old version stays loaded when the new one is refused.
    pub fn reload_package(&self, wasm: &[u8], meta: Meta) -> Result<PackageId, RuntimeError> {
        let hash = self.core.engine.compile(wasm)?;
        // The version loaded now brings its own words: a package reloaded after its catalogues
        // were translated reads them, not the ones it was loaded with before. The window's own
        // package still chooses the language; another app's, running here inline, rides along.
        let role = if self
            .core
            .registry
            .borrow()
            .packages()
            .iter()
            .any(|package| package.foreign && package.meta.app == meta.app)
        {
            crate::words::WordsRole::Used
        } else {
            crate::words::WordsRole::Window
        };
        self.add_catalogues(wasm, role);
        self.check_exports(&hash, &meta)?;
        let lines = tilolang::meta::lines_from_wasm(wasm);
        let pkg = crate::registry::Package::with_lines(hash.clone(), meta, &lines)
            .map_err(RuntimeError::Package)?;
        self.core
            .registry
            .borrow_mut()
            .replace(pkg)
            .map_err(RuntimeError::Package)?;
        self.publish_types();
        Ok(hash)
    }

    /// Give the store the types every loaded package declares: the fields a record's links are
    /// read by.
    fn publish_types(&self) {
        let types = self.core.registry.borrow().loaded_types();
        self.core.store.borrow_mut().loaded_types(&types);
    }

    fn check_exports(&self, hash: &ModuleHash, meta: &Meta) -> Result<(), RuntimeError> {
        let exports = self.core.engine.exports(hash);
        let wanted = meta
            .actions
            .iter()
            .map(|a| format!("a_{}", a.id))
            .chain(meta.views.iter().map(|v| format!("v_{}", v.id)))
            .chain(meta.fns.iter().map(|f| format!("n_{}", f.id)))
            .chain(
                meta.at_event
                    .iter()
                    .map(|f| tilolang::ir::at_event_export(&f.id)),
            )
            .chain(meta.actions.iter().flat_map(|a| {
                a.params
                    .iter()
                    .filter_map(|p| p.default.as_ref().map(|d| d.export.clone()))
            }))
            .chain(
                meta.actions
                    .iter()
                    .filter(|a| a.guard)
                    .map(|a| tilolang::ir::guard_export(&a.id)),
            )
            .chain(
                meta.actions
                    .iter()
                    .filter(|a| a.applies)
                    .map(|a| tilolang::ir::applies_export(&a.id)),
            );
        for name in wanted {
            if !exports.contains(&name) {
                return Err(RuntimeError::Package(format!(
                    "the module does not export `{name}`"
                )));
            }
        }
        Ok(())
    }

    /// Load a package the compiler produced.
    ///
    /// # Errors
    ///
    /// As [`Runtime::load_package`].
    pub fn load_compiled(&self, compiled: &Compiled) -> Result<PackageId, RuntimeError> {
        self.load_package(&compiled.wasm, compiled.meta.clone())
    }

    /// Load a package from its module alone: the metadata is read from the module's
    /// `tilolang.meta` section.
    ///
    /// # Errors
    ///
    /// `Package` when the module carries no readable metadata; otherwise as
    /// [`Runtime::load_package`].
    pub fn load_wasm(&self, wasm: &[u8]) -> Result<PackageId, RuntimeError> {
        let meta = Meta::from_wasm(wasm).ok_or_else(|| {
            RuntimeError::Package("the module has no readable tilolang.meta".into())
        })?;
        self.load_package(wasm, meta)
    }

    /// Load the description of an app this runtime does not run: its types, actions and views,
    /// and no code. A view takes a resource of it as a parameter, its fields read from the
    /// readings its app publishes ([`Runtime::set_reading`]); an action of it is a child
    /// invocation at the app's own door ([`crate::delegate`], [`Runtime::set_invoker`]).
    ///
    /// The package is identified by the hash of the description, which stands for the app's
    /// code authorship until a newer description of it replaces it.
    ///
    /// # Errors
    ///
    /// `Package` for a description that does not read, or one that reuses another package's
    /// declaration ids.
    pub fn load_foreign(&self, meta: Meta) -> Result<PackageId, RuntimeError> {
        let app = meta.app.clone();
        let hash = ModuleHash::of_bytes(&meta.to_json_bytes());
        let pkg = crate::registry::Package::foreign(hash, meta).map_err(RuntimeError::Package)?;
        let hash = self
            .core
            .registry
            .borrow_mut()
            .add(pkg)
            .map(|pkg| pkg.hash.clone())
            .map_err(RuntimeError::Package)?;
        self.publish_types();
        self.package_arrived(&app);
        Ok(hash)
    }

    /// Load another app's package with its module: its views run here, inline in the views
    /// that call them (`mail.inbox(m)`, or `show(r)` by the resource's type), as that package's own code,
    /// so what they create and invoke carries its module as the author, never the caller's.
    /// Its records stay its app's: a view of it reads a resource from the reading its app
    /// publishes ([`Runtime::set_reading`]), named by app and type (`mail#Inbox`), and its
    /// actions are child invocations at that app's door ([`Runtime::set_invoker`]).
    ///
    /// # Errors
    ///
    /// As [`Runtime::load_package`].
    pub fn load_foreign_package(&self, wasm: &[u8], meta: Meta) -> Result<PackageId, RuntimeError> {
        let hash = self.core.engine.compile(wasm)?;
        self.add_catalogues(wasm, crate::words::WordsRole::Used);
        self.check_exports(&hash, &meta)?;
        let app = meta.app.clone();
        let pkg = crate::registry::Package::foreign_with_code(hash.clone(), meta)
            .map_err(RuntimeError::Package)?;
        self.core
            .registry
            .borrow_mut()
            .add(pkg)
            .map_err(RuntimeError::Package)?;
        self.publish_types();
        self.package_arrived(&app);
        Ok(hash)
    }

    /// Another app's package is loaded: a view call that read a resource of a type no package
    /// declared until now runs again, and draws it through that app's own views.
    fn package_arrived(&self, app: &str) {
        let arrivals = self.core.wanted_packages.borrow_mut().arrived(app);
        // A package that failed to load here and now arrived: its views draw again, and the
        // failure that stood in for them is forgotten.
        self.core.failed_views.borrow_mut().app_arrived(app);
        self.core.view_states.borrow_mut().set(
            crate::foreign::packages_key(),
            Value::Int(i64::try_from(arrivals).unwrap_or(i64::MAX)),
        );
    }

    /// Unload the app `app` this runtime does not run, which nothing here shows any more: its
    /// package (description or code) is declared here no more, and the readings of its
    /// resources are forgotten. Whether it was loaded.
    pub fn unload_foreign(&self, app: &str) -> bool {
        let removed = self.core.registry.borrow_mut().remove_foreign(app);
        self.core.foreign.borrow_mut().forget_app(app);
        self.core.wanted_packages.borrow_mut().forget(app);
        removed
    }

    /// The resources of apps whose code runs here (`Runtime::load_foreign_package`) that a
    /// view read before their readings came in, each its id and its type (`<app>#<Type>`):
    /// what the host asks the session to publish ([`Runtime::set_reading`]). Each is handed
    /// out once; until its reading comes, it reads as it would start.
    pub fn take_wanted(&self) -> Vec<(String, String)> {
        self.core.foreign.borrow_mut().take_wanted()
    }

    /// The apps whose package a view read a resource of before it was loaded here, each with
    /// the type as the view named it (`<app>#<Type>`): what the host loads
    /// ([`Runtime::load_foreign_package`]). Each app is asked for once; until its package
    /// comes, the view that read it shows its waiting state.
    pub fn take_wanted_packages(&self) -> Vec<(String, String)> {
        self.core.wanted_packages.borrow_mut().take()
    }

    /// Whether a view waits on the package of an app no host has served yet.
    pub fn wants_packages(&self) -> bool {
        self.core.wanted_packages.borrow().wants()
    }

    /// The view calls that could not be shown since the last call — their package did not load
    /// here, or their view failed when it ran — each an app, a view and why
    /// ([`crate::foreign::ViewFailure`]). What the host says the person should hear about
    /// (`shell.md` §6, "Nothing can lock a person out"). Each is handed out once.
    pub fn take_view_failures(&self) -> Vec<crate::foreign::ViewFailure> {
        self.core.failed_views.borrow_mut().take()
    }

    /// The host could not load the package of `app`, for `reason`: a view of its that a screen
    /// calls draws a failed state naming it rather than waiting for a package that is not
    /// coming. [`Runtime::app_loaded`] clears it when the package does load.
    pub fn app_failed(&self, app: &str, reason: String) {
        self.core.failed_views.borrow_mut().app_failed(app, reason);
    }

    /// The apps whose package failed to load and then loaded here: what the host says the
    /// person's choice is back (`shell.md` §6). Each once.
    pub fn take_recovered_apps(&self) -> Vec<String> {
        self.core.failed_views.borrow_mut().take_recovered()
    }

    /// The view calls whose failure ended — the call ran again and succeeded: what the host
    /// says the person's view is back (`shell.md` §6). Each once.
    pub fn take_recovered_views(&self) -> Vec<crate::foreign::ViewFailure> {
        self.core.failed_views.borrow_mut().take_recovered_views()
    }

    /// Whether `error` — a view call's failure — was waiting on a reading that has not arrived
    /// yet (a place whose app publishes nothing yet, or whose record the session has not
    /// published): the call failed for want of data, not for a fault. A host shows that call's
    /// place its waiting state and draws the rest of the screen; the call runs again when the
    /// reading comes (`Runtime::set_reading`).
    pub fn waiting(&self, error: &str) -> bool {
        self.core.foreign.borrow().waiting(error)
    }

    /// Forget the waiting errors gathered so far: a turn's tree is built afresh, and what it
    /// gathers is what it failed for.
    pub fn clear_waiting(&self) {
        self.core.foreign.borrow().take_waiting();
    }

    /// The reading of `target`, a resource of an app this runtime does not run, whole, with the
    /// generation it was read at: what a Rust app's view is handed of the resource it shows.
    ///
    /// `None` before a reading arrived (the host is asked to publish one, as a field read does),
    /// and for a resource this runtime holds: its own records run from here.
    #[must_use]
    pub fn reading(&self, target: &ResourceRef) -> Option<(serde_json::Value, u64)> {
        if !crate::foreign::is_foreign(target) {
            return None;
        }
        self.core.foreign.borrow_mut().whole(target)
    }

    /// The reference `target` names, with its type filled in when it came in by its id alone: a
    /// resource another view dragged onto this one (a chip on a composer, a place's handle on
    /// the canvas) is carried as its published address and nothing more, so the type is read
    /// from the record this runtime holds, else from the reading its app published of it.
    ///
    /// `None` for an address no record holds and no reading names yet — a resource that is
    /// gone, or not synced here. A target that already names its type is returned as it is.
    #[must_use]
    pub fn resolve_ref(&self, target: &ResourceRef) -> Option<ResourceRef> {
        if !target.ty.is_empty() {
            return Some(target.clone());
        }
        self.core.resolve_ref(target)
    }

    /// Tell the runtime the views of a Rust app: its registry, so a `show` of a resource of one of
    /// its types draws that app's view inline, as it draws a tilolang package's view
    /// ([`crate::rust_views`]). The host that runs the app calls this once.
    pub fn set_rust_views(&self, views: std::rc::Rc<dyn crate::rust_views::RustViews>) {
        let app = views.app().to_owned();
        self.core.rust_views.borrow_mut().insert(app, views);
    }

    /// The reading of `target`, a resource of an app this runtime does not run, as the
    /// session's watch last saw it: the JSON its service read. Views that read a field of it
    /// run again at the next poll. Whether it differs from the reading held before: `false`
    /// when nothing moved.
    ///
    /// # Errors
    ///
    /// `Package` when `target` is a resource this runtime holds (it reads its own record), or
    /// the text is not a reading (JSON holding an object).
    pub fn set_reading(&self, target: &ResourceRef, reading: &str) -> Result<bool, RuntimeError> {
        if !crate::foreign::is_foreign(target) {
            return Err(RuntimeError::Package(format!(
                "`{}` is held here: its fields run from its own record",
                target.id_string()
            )));
        }
        let json: Json = serde_json::from_str(reading).map_err(|error| {
            RuntimeError::Package(format!("the reading of `{}`: {error}", target.id_string()))
        })?;
        if !json.is_object() {
            return Err(RuntimeError::Package(format!(
                "the reading of `{}` is not an object of its fields",
                target.id_string()
            )));
        }
        let mut foreign = self.core.foreign.borrow_mut();
        let first = foreign.generation(target) == 0;
        if !foreign.set_reading(target, json) {
            return Ok(false);
        }
        let generation = foreign.generation(target);
        let known = foreign.known();
        drop(foreign);
        let seen = i64::try_from(generation).unwrap_or(i64::MAX);
        let mut states = self.core.view_states.borrow_mut();
        states.set(crate::foreign::reading_key(target), Value::Int(seen));
        // Any reading that comes in: a view call waiting on one it could not know runs again.
        states.set(crate::foreign::arrived_key(), Value::Int(seen));
        // A resource known here for the first time: views that looked it up by id
        // (`resource(id)`) and found nothing run again.
        if first {
            states.set(
                crate::foreign::known_key(),
                Value::Int(i64::try_from(known).unwrap_or(i64::MAX)),
            );
        }
        Ok(true)
    }

    /// Evaluate an agent block as `principal`. A view renders and
    /// can be pressed; an action application is invoked, or suggested when the authority asks
    /// (the block's module is the author, so the chain starts with it); data is returned.
    ///
    /// # Errors
    ///
    /// `Package` for a module that does not link, a trap, or a value of the wrong type.
    pub fn run_block(
        &self,
        principal: &Principal,
        block: &CompiledBlock,
    ) -> Result<BlockOutput, RuntimeError> {
        let pkg = self.core.load_block(&block.wasm, &block.meta)?;
        match block.kind {
            BlockKind::View => {
                let target = crate::render::ViewTarget {
                    module: pkg.hash.clone(),
                    export: "b_main".to_owned(),
                    view: String::new(),
                    params: Vec::new(),
                };
                let window = crate::view_instance::default_window(&target, &[]);
                let view = ViewInstance::open(
                    Rc::clone(&self.core),
                    (principal.clone(), window),
                    target,
                    Vec::new(),
                )?;
                Ok(BlockOutput::View(Box::new(view)))
            }
            BlockKind::Action => {
                let Value::Action(aref) = self.core.eval_block(&pkg, &block.value_type, None)?
                else {
                    return Err(RuntimeError::Package(
                        "the block did not produce an action".into(),
                    ));
                };
                Ok(BlockOutput::Action(
                    self.core.invoke_direct(principal, &aref)?,
                ))
            }
            BlockKind::Data => Ok(BlockOutput::Data(self.core.eval_block(
                &pkg,
                &block.value_type,
                None,
            )?)),
        }
    }

    /// Evaluate the source of a block an agent wrote, as `agent`: a view renders, a value is
    /// projected, and an action application runs or is suggested to a person, recorded as an
    /// `Invocation` resource.
    ///
    /// # Errors
    ///
    /// A store failure while recording the invocation, or a block that traps.
    pub fn run_agent_block(
        &self,
        agent: &Principal,
        source: &str,
    ) -> Result<AgentBlock, RuntimeError> {
        self.core.run_agent_block(agent, source)
    }

    /// What the written view `source` by `author` is (`tilolang::written`): checked against
    /// every package this runtime holds, so it reads any type the workspace has installed.
    pub fn check_written_view(
        &self,
        source: &str,
        author: &Principal,
    ) -> Result<crate::written::WrittenViewInfo, Vec<tilolang::Diagnostic>> {
        self.core.check_written_view(source, author)
    }

    /// Open the written view `source` for `viewer` over `args`: it reads what `viewer` reads,
    /// and `author`, who wrote it, is its code author, so what it does with an action asks first
    /// unless the viewer trusts them.
    ///
    /// # Errors
    /// `Package` when it does not compile or takes other arguments; otherwise as
    /// [`Runtime::open_view`].
    pub fn open_written_view(
        &self,
        viewer: &Principal,
        source: &str,
        author: &Principal,
        args: Vec<Value>,
    ) -> Result<ViewInstance, RuntimeError> {
        self.core.open_written_view(viewer, source, author, args)
    }

    /// Record the suggestion raised for `action` called with `args` by `invoker` as an
    /// `Invocation` resource that waits for a person.
    ///
    /// # Errors
    ///
    /// A store failure while recording.
    pub fn record_suggestion(
        &self,
        invoker: &Principal,
        action: &str,
        args: Vec<Value>,
        suggestion: &crate::run::Suggestion,
    ) -> Result<ResourceRef, RuntimeError> {
        let aref = ActionRef {
            action: action.to_owned(),
            args,
            author: None,
            from: None,
        };
        self.core.record_suggestion(invoker, &aref, suggestion)
    }

    /// Decide the suggestion an `Invocation` resource holds (see `Core::resolve_invocation`):
    /// approving runs the action once, on this replica, and records the outcome in the
    /// resource so every replica shows it after sync. Returns the invocation's new state.
    ///
    /// # Errors
    ///
    /// `NotPending` when it is not a suggestion, `Denied` when the authority gives `approver`
    /// no say, otherwise as running the action.
    pub fn resolve_invocation(
        &self,
        invocation: &ResourceRef,
        approver: &Principal,
        resolution: Resolution,
    ) -> Result<String, RuntimeError> {
        self.core
            .resolve_invocation(invocation, approver, resolution)
    }

    /// The proposals waiting on `resource`: each the `Invocation` it waits in and what it says,
    /// with the person or agent who proposed it. Empty when nobody proposed anything that is
    /// still undecided.
    ///
    /// # Errors
    ///
    /// Never for a resource that exists; a store that cannot be read says nothing waits.
    #[must_use]
    pub fn proposals_of(
        &self,
        resource: &ResourceRef,
    ) -> Vec<(ResourceRef, crate::InvocationRecord)> {
        self.core.proposals_of(resource)
    }

    /// What the host recorded for an `Invocation` resource.
    ///
    /// # Errors
    ///
    /// `NotFound` when the resource is not an invocation.
    pub fn read_invocation(
        &self,
        invocation: &ResourceRef,
    ) -> Result<InvocationRecord, RuntimeError> {
        self.core.read_invocation(invocation)
    }

    /// Create a resource of a type, by id or by a name one loaded package declares.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown or ambiguous type, or a store failure.
    pub fn create(&self, ty: &str) -> Result<ResourceRef, RuntimeError> {
        let record = {
            let reg = self.core.registry.borrow();
            let id = reg.resolve_global(ty).map_err(RuntimeError::NotFound)?;
            reg.new_record(&id)
                .ok_or_else(|| RuntimeError::NotFound(format!("type {ty}")))?
        };
        Ok(self.core.store.borrow_mut().create(&record)?)
    }

    /// Create the per-workspace root of a type, by id or by a name one loaded package declares: the
    /// resource `root T` and a named root reach (`root-<type id>`).
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown or ambiguous type; a store failure, or a root that exists.
    pub fn create_root(&self, ty: &str) -> Result<ResourceRef, RuntimeError> {
        let record = {
            let reg = self.core.registry.borrow();
            let id = reg.resolve_global(ty).map_err(RuntimeError::NotFound)?;
            reg.new_record(&id)
                .ok_or_else(|| RuntimeError::NotFound(format!("type {ty}")))?
        };
        let id = format!("root-{}", record.ty);
        Ok(self.core.store.borrow_mut().create_with_id(&record, &id)?)
    }

    /// Read a field of a resource, by field id or name. A resource of an app this runtime does
    /// not run reads its field from the reading its app publishes, as a view's code does.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown type or field; a store failure, or a reading that does not fit
    /// the type the app declares.
    pub fn read(&self, at: &ResourceRef, field: &str) -> Result<Value, RuntimeError> {
        let (_, info) = self
            .core
            .type_of_ref(&at.ty)
            .map_err(RuntimeError::NotFound)?;
        let f = info
            .field_by_id_or_name(field)
            .ok_or_else(|| {
                RuntimeError::NotFound(format!(
                    "field `{field}` of `{}`; fields are: {}",
                    info.name,
                    info.field_names()
                ))
            })?
            .clone();
        self.core
            .read_value(at, &f, None)
            .map_err(RuntimeError::Package)
    }

    /// What `at` is called, the way every surface that names it shows it: its title while it
    /// has one, else the words its type declares for it while untitled, else the package's
    /// title for a root, else its type's name.
    ///
    /// # Errors
    ///
    /// `NotFound` for a type this runtime does not run; a store failure, or a reading that
    /// does not fit the type the app declares.
    pub fn title(&self, at: &ResourceRef) -> Result<String, RuntimeError> {
        let (pkg, info) = self
            .core
            .type_of_ref(&at.ty)
            .map_err(RuntimeError::NotFound)?;
        let declared = pkg
            .meta
            .types
            .iter()
            .find(|ty| ty.id == info.id)
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", at.ty)))?;
        self.core
            .title_of(at, &pkg.meta, declared)
            .map_err(|fail| match fail {
                crate::engine::HostFail::Trap(message) => RuntimeError::Package(message),
                crate::engine::HostFail::Suspend => RuntimeError::Package("suspended".to_owned()),
            })
    }

    /// Invoke an action as `principal`. The action is an [`ActionRef`], or an
    /// `(action id, args)` pair for a call made straight by the principal.
    pub fn invoke_action(
        &self,
        principal: &Principal,
        action: impl Into<ActionRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.core.invoke_direct(principal, &action.into())
    }

    /// [`Runtime::invoke_action`] as the run `run`: an invocation every replica names the same
    /// way — a trigger run for a record at an instant, on whichever device comes to run it —
    /// runs as that run on each of them, so the records it writes and the effects it queues are
    /// the same, and the workspace holds each of them once.
    ///
    /// # Errors
    ///
    /// As [`Runtime::invoke_action`].
    pub fn invoke_action_named(
        &self,
        run: &RunId,
        principal: &Principal,
        action: impl Into<ActionRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.core
            .invoke_direct_named(run, principal, &action.into())
    }

    /// [`Runtime::invoke_action`] made from the resource `from`: what the resources the action
    /// creates were made from, recorded in the change it makes (`created(r)?.from`,
    /// [`crate::Store::linked`]). What a "make from this" menu, or an agent working from
    /// something it read, sets.
    ///
    /// # Errors
    ///
    /// As [`Runtime::invoke_action`].
    pub fn invoke_action_from(
        &self,
        principal: &Principal,
        action: impl Into<ActionRef>,
        from: &crate::value::ResourceRef,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.core
            .invoke_direct_from(principal, &action.into(), Some(from))
    }

    /// The arguments of an invocation of `action` with those left out (`None`) given their
    /// defaults: each computed now, on the current state, over the arguments before it. What an
    /// invocation records, so that replay never computes a default again.
    ///
    /// # Errors
    ///
    /// An unknown action, an argument left out that has no default, or a default that traps.
    pub fn complete_args(
        &self,
        action: &str,
        given: Vec<Option<Value>>,
    ) -> Result<Vec<Value>, RuntimeError> {
        self.core.complete_args(action, given)
    }

    /// Fills the parameters `given` leaves out of `action` from `text`, the one line a person
    /// typed for it: the entry parameter with the words nothing else took, and the parameters
    /// their types read (a day, a moment, a person, a record). The words name a parameter over
    /// the value the call gave it; that value stands where they say nothing.
    /// [`crate::Runtime::complete_args`] then defaults the rest. Returns the arguments as the
    /// call carries them — as long as the action's parameters, what the call left out
    /// [`None`] — and `None` when the line names nothing, as an entry's text that is only
    /// space and what other parameters take makes no item.
    pub fn fill_entry(
        &self,
        action: &str,
        given: Vec<Option<Value>>,
        text: &str,
    ) -> Result<Option<Vec<Option<Value>>>, RuntimeError> {
        self.core
            .entry_fill_given(action, given, text)
            .map_err(|fail| match fail {
                crate::engine::HostFail::Trap(message) => RuntimeError::Package(message),
                // Filling arguments reads the text and the workspace, never asks a question.
                crate::engine::HostFail::Suspend => {
                    RuntimeError::Package("reading what was typed asked for a decision".to_owned())
                }
            })
    }

    /// The default of the parameter `at` of `action`, computed now over the arguments before
    /// it: what its form starts filled in with. `None` when it has none.
    pub fn default_argument(
        &self,
        action: &str,
        at: usize,
        before: &[Value],
    ) -> Result<Option<Value>, RuntimeError> {
        self.core.default_argument(action, at, before)
    }

    /// The workspace's members as the host last gave them ([`Self::set_members`]).
    #[must_use]
    pub fn members(&self) -> Vec<crate::members::Member> {
        self.core.members.borrow().list().to_vec()
    }

    /// When `text` says something is due ("tomorrow 9"), read now in the person's zone.
    #[must_use]
    pub fn dated(&self, text: &str) -> tilolang::natural::Dated {
        let now = tilolang::calendar::instant(&self.core.clock_now()).unwrap_or(0);
        let offset = self.core.zone.borrow().as_ref().map_or(0, |zone| zone());
        tilolang::natural::dated(text, now, offset)
    }

    /// Answer the suggestion a run waits on and continue the run.
    ///
    /// # Errors
    ///
    /// `NotPending` when nothing waits on the run.
    pub fn resolve(
        &self,
        run: &RunId,
        approver: &Principal,
        resolution: Resolution,
    ) -> Result<crate::run::RunOutcome, RuntimeError> {
        self.core.resolve(run, approver, resolution)
    }

    /// Open a view for `principal`, in the window of its first resource argument.
    pub fn open_view(
        &self,
        principal: &Principal,
        view: &str,
        args: Vec<Value>,
    ) -> Result<ViewInstance, RuntimeError> {
        let target = self.core.view_target(view)?;
        let window = crate::view_instance::default_window(&target, &args);
        ViewInstance::open(
            Rc::clone(&self.core),
            (principal.clone(), window),
            target,
            args,
        )
    }

    /// Open a view for `principal` in the window `window`: the state it declares is how their
    /// window shows the resource and where they are in it, that person's own, so another person
    /// looking at the same window is not moved by it.
    pub fn open_window(
        &self,
        principal: &Principal,
        window: &str,
        view: &str,
        args: Vec<Value>,
    ) -> Result<ViewInstance, RuntimeError> {
        let target = self.core.view_target(view)?;
        ViewInstance::open(
            Rc::clone(&self.core),
            (principal.clone(), window.to_owned()),
            target,
            args,
        )
    }

    /// The view a window of the resource type `ty` (an id) shows by default.
    pub fn default_view(&self, ty: &str) -> Option<String> {
        self.core.registry.borrow().default_view(ty)
    }

    /// Open the window of `resource` for `principal` (what `open(r)` asks for): through
    /// `view`, or the default view of its type.
    ///
    /// # Errors
    ///
    /// `NotFound` when the type has no default view and none is given; otherwise as
    /// [`Runtime::open_window`].
    pub fn open_resource(
        &self,
        principal: &Principal,
        resource: &ResourceRef,
        view: Option<&str>,
    ) -> Result<ViewInstance, RuntimeError> {
        let view = match view {
            Some(v) => v.to_owned(),
            None => self.default_view(&resource.ty).ok_or_else(|| {
                RuntimeError::NotFound(format!("a default view for type {}", resource.ty))
            })?,
        };
        self.open_window(
            principal,
            &resource.id_string(),
            &view,
            vec![Value::resource(resource)],
        )
    }

    /// What an agent sees of a value ([`crate::project`]).
    pub fn project(&self, value: &Value) -> Json {
        project(&self.core, value, ProjectOptions::default())
    }

    /// As [`Runtime::project`] with explicit limits.
    pub fn project_with(&self, value: &Value, opts: ProjectOptions) -> Json {
        project(&self.core, value, opts)
    }

    /// Why the action `action` cannot run on the resource `subject` now, as the requirements its
    /// subject alone decides say: the first that does not hold, in `viewer`'s words. `None` while
    /// it can run, and for an action whose app keeps no requirement the subject decides.
    pub fn refusal(
        &self,
        action: &str,
        subject: &ResourceRef,
        viewer: &Principal,
    ) -> Result<Option<String>, RuntimeError> {
        let subject = crate::value::RefValue {
            target: Rc::new(subject.clone()),
            author: None,
        };
        self.core
            .refusal(action, &subject, viewer)
            .map_err(|error| match error {
                crate::engine::HostFail::Trap(message) => RuntimeError::Package(message),
                crate::engine::HostFail::Suspend => {
                    RuntimeError::Package("a guard waited on a decision".into())
                }
            })
    }

    /// As [`Runtime::refusal`], with what reading the requirement read: the fields it read, each
    /// once, and whether it read more than the workspace holds (the clock, the window's space,
    /// the person's view state), which no change in the workspace moves.
    pub fn refusal_reading(
        &self,
        action: &str,
        subject: &ResourceRef,
        viewer: &Principal,
    ) -> Result<(Option<String>, Reading), RuntimeError> {
        let subject = crate::value::RefValue {
            target: Rc::new(subject.clone()),
            author: None,
        };
        self.core
            .refusal_reading(action, &subject, viewer)
            .map_err(|error| match error {
                crate::engine::HostFail::Trap(message) => RuntimeError::Package(message),
                crate::engine::HostFail::Suspend => {
                    RuntimeError::Package("a guard waited on a decision".into())
                }
            })
    }

    /// As [`Runtime::refusal`], with what a view's offer of the action reads: whether the action
    /// can apply to `subject` where it is offered at all, and the first reason it cannot.
    pub fn applies_reading(
        &self,
        action: &str,
        subject: &ResourceRef,
        viewer: &Principal,
    ) -> Result<(Option<String>, Reading), RuntimeError> {
        let subject = crate::value::RefValue {
            target: Rc::new(subject.clone()),
            author: None,
        };
        self.core
            .applies_reading(action, &subject, viewer)
            .map_err(|error| match error {
                crate::engine::HostFail::Trap(message) => RuntimeError::Package(message),
                crate::engine::HostFail::Suspend => {
                    RuntimeError::Package("an applies condition waited on a decision".into())
                }
            })
    }

    /// Signatures of every loaded package, for discovery ([`catalogue`](mod@crate::catalogue)).
    pub fn catalogue(&self) -> Vec<Signature> {
        let pkgs = self.core.registry.borrow().packages();
        let metas: Vec<&Meta> = pkgs.iter().map(|p| &p.meta).collect();
        catalogue(&metas)
    }

    /// Where `web.get` and `web.post` reach: the network of the device whose code makes the
    /// request, which runs each request once, and whose journal entry a replay answers from.
    pub fn set_web(&self, transport: impl crate::web::WebTransport + 'static) {
        *self.core.web.borrow_mut() = Some(Box::new(transport));
    }

    /// Where `model_call` goes.
    pub fn set_model_host(&self, host: impl ModelHost + 'static) {
        *self.core.models.borrow_mut() = Some(Box::new(host));
    }

    /// How views show a principal's name. Until it is set, a principal is its id.
    pub fn set_name_host(&self, host: impl crate::core::NameHost + 'static) {
        *self.core.names.borrow_mut() = Some(Box::new(host));
    }

    /// How a day or a time of day reads in a sentence (a row's line, a notice) rather than in a
    /// view node. Until it is set, its stored text.
    pub fn set_shown_host(&self, host: impl crate::core::ShownHost + 'static) {
        *self.core.shown.borrow_mut() = Some(Box::new(host));
    }

    /// What `now` reads: the current time as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`. The
    /// runtime reads it once per outermost invocation, so the actions one invocation runs see
    /// one time. Until it is set, `now` is `1970-01-01T00:00:00Z`.
    pub fn set_clock(&self, clock: impl Fn() -> String + 'static) {
        *self.core.clock.borrow_mut() = Some(Box::new(clock));
    }

    /// How many seconds the person's clock is ahead of UTC: the zone `dated` reads "tomorrow
    /// 9am" in. Until it is set, UTC.
    pub fn set_zone(&self, zone: impl Fn() -> i64 + 'static) {
        *self.core.zone.borrow_mut() = Some(Box::new(zone));
    }

    /// The person's zone as its IANA name (`Europe/Berlin`): a repeating item keeps its wall time
    /// of day across a change of clocks in it, and a day begins there first. Until it is set, a
    /// day and a moment are read by [`Runtime::set_zone`]'s offset, and an occurrence keeps the
    /// time of day that offset gives it.
    pub fn set_zone_name(&self, zone: impl Fn() -> Option<String> + 'static) {
        *self.core.zone_name.borrow_mut() = Some(Box::new(zone));
    }

    /// The day the person's region starts its week on, as days from Monday (`0`): the day a
    /// day's `start_of_week` goes back to. Monday until it is set.
    pub fn set_week_start(&self, start: impl Fn() -> i64 + 'static) {
        *self.core.week_start.borrow_mut() = Some(Box::new(start));
    }

    /// Where the words packages and the platform show come from, in the reader's language
    /// (`phrase(id, ...)`, a declaration's label and noun). It takes every package's catalogues
    /// loaded so far, and those loaded after: the window's own package's as the word `Words::add`
    /// takes them, a package's it uses as `Words::add_used` does. Until it is set, the source
    /// words stand.
    pub fn set_words(&self, words: impl crate::words::Words + 'static) {
        for (role, catalogues) in self.core.catalogues.borrow().iter() {
            role.hand(&words, catalogues);
        }
        *self.core.words.borrow_mut() = Box::new(words);
    }

    /// Where the words packages and the platform show come from now, in the reader's language:
    /// what a published label, description or noun reads through
    /// ([`crate::words::Words::word`]).
    #[must_use]
    pub fn words(&self) -> std::cell::Ref<'_, dyn crate::words::Words> {
        std::cell::Ref::map(self.core.words.borrow(), |words| &**words)
    }

    /// Takes in the catalogues the module `wasm` carries, as `role` says which package of the
    /// window they belong to.
    fn add_catalogues(&self, wasm: &[u8], role: crate::words::WordsRole) {
        let catalogues = tilolang::meta::locales_from_wasm(wasm);
        if catalogues.is_empty() {
            return;
        }
        role.hand(&**self.core.words.borrow(), &catalogues);
        self.core.catalogues.borrow_mut().push((role, catalogues));
    }

    /// What a reference to a resource the person cannot read says, in their language: the
    /// chip's wording (`tilo_reference::embed::UNSHARED`).
    #[must_use]
    pub fn unshared(&self) -> String {
        self.core.unshared()
    }

    /// The lenses on the resource `resource` (its id) now, as presence reports them: who,
    /// whether it is the person looking, and whether they are composing a message in it. What
    /// `lenses(r)` reads; a view that read the lenses on it runs again when they move.
    pub fn set_lenses(&self, resource: &str, lenses: &[(Principal, bool, bool)]) {
        let key = crate::platform::lens_key(resource);
        let value = Value::List(
            lenses
                .iter()
                .map(|(who, mine, composing)| {
                    Value::Tuple(vec![
                        Value::Principal(who.clone()),
                        Value::Bool(*composing),
                        Value::Bool(*mine),
                    ])
                })
                .collect(),
        );
        let mut states = self.core.view_states.borrow_mut();
        if states.get(&key) != Some(&value) {
            states.set(key, value);
        }
    }

    /// `person`'s seen mark of the resource `resource` (its id), as their own record keeps it
    /// ([`crate::seen`]): what `unseen(r)` and the marks the host draws read. A view that read
    /// it runs again when it moves.
    pub fn set_seen(&self, person: &str, resource: &str, mark: Option<crate::seen::SeenMark>) {
        let key = crate::seen::seen_key(person, resource);
        let value = mark.map_or_else(Value::none, |mark| {
            Value::some(Value::Tuple(vec![
                Value::Text(mark.change),
                Value::Text(mark.at),
            ]))
        });
        let mut states = self.core.view_states.borrow_mut();
        if states.get(&key) != Some(&value) {
            states.set(key, value);
        }
    }

    /// `person`'s seen mark of `resource`, as this runtime holds it.
    #[must_use]
    pub fn seen(&self, person: &str, resource: &str) -> Option<crate::seen::SeenMark> {
        self.core.seen_mark(person, resource)
    }

    /// The versions of `at` that `person` has not seen, oldest first: what `unseen(r)` reads
    /// for them.
    ///
    /// # Errors
    ///
    /// The store could not read the versions of `at`.
    pub fn unseen(&self, person: &str, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        self.core.unseen_of(person, at)
    }

    /// `person` has seen every version of `at` there is now: the mark moves to its newest,
    /// and the host is handed it to keep ([`Self::take_seen`]). Nothing happens while there is
    /// nothing unseen.
    ///
    /// # Errors
    ///
    /// The store could not read the versions of `at`.
    pub fn mark_seen(&self, person: &str, at: &ResourceRef) -> Result<bool, StoreError> {
        self.core.mark_seen(person, at)
    }

    /// The runs `person` reviewed, as their own record keeps them: every version of one is
    /// seen. A view that read what is unseen runs again when they move.
    pub fn set_seen_runs(&self, person: &str, runs: &[String]) {
        self.core.set_seen_runs(person, runs);
    }

    /// `person` reviewed the run `run` (its group, *Mark all seen*): every version it made
    /// is seen, in every app, and the host is handed it to keep ([`Self::take_seen`]). Whether
    /// it was not reviewed already.
    pub fn mark_run_seen(&self, person: &str, run: &str) -> bool {
        let mut runs = self.core.seen_runs(person);
        if runs.iter().any(|seen| seen == run) {
            return false;
        }
        runs.push(run.to_owned());
        self.core.set_seen_runs(person, &runs);
        self.core
            .seen_writes
            .borrow_mut()
            .push(crate::seen::SeenWrite::Run {
                person: person.to_owned(),
                run: run.to_owned(),
            });
        true
    }

    /// `person` marked everything they had not seen of `resources` seen, in one move (*Mark
    /// all seen*): each is seen up to its newest version, what it holds read to as well, and
    /// the host is handed it to keep ([`Self::take_seen`]). How many marks it moved; the move
    /// can be taken back with [`Self::undo_seen`].
    ///
    /// # Errors
    ///
    /// The store could not read the versions of one of them.
    pub fn mark_all_seen(&self, person: &str, resources: &[String]) -> Result<usize, StoreError> {
        self.core.mark_all_seen(person, resources)
    }

    /// Give back what the last catch-up in this runtime took away: every mark it moved, as it
    /// was, and the host is handed it to keep. Whether there was one to take back.
    pub fn undo_seen(&self, person: &str) -> bool {
        self.core.undo_seen(person)
    }

    /// The seen marks set here since last asked, for the host to keep in the person's own
    /// record.
    pub fn take_seen(&self) -> Vec<crate::seen::SeenWrite> {
        std::mem::take(&mut *self.core.seen_writes.borrow_mut())
    }

    /// The view state the person set in views since last asked, for the host to
    /// keep in their own record: one entry per slot, as it is now.
    pub fn take_view_writes(&self) -> Vec<(crate::view_state::StateKey, Value)> {
        self.core.view_states.borrow_mut().take_writes()
    }

    /// Put back a slot of view state the person's record keeps: what they picked
    /// here before, or on another device of theirs. A slot already holding it is left alone.
    pub fn set_view_state(&self, key: crate::view_state::StateKey, value: Value) -> bool {
        self.core.view_states.borrow_mut().restore(key, value)
    }

    /// Stage the value the view state `state` of the app `app` starts from, before anyone
    /// picks there: `text` names the value as the app's own code writes it (`Task.today`,
    /// `Side.right`), read against the state's declared type. A window that reads the state
    /// shows it until its person picks there, and it is no one's change to keep.
    pub fn stage_view_state(&self, app: &str, state: &str, text: &str) -> Result<(), String> {
        let (id, value) = {
            let reg = self.core.registry.borrow();
            let pkg = reg
                .packages()
                .into_iter()
                .find(|pkg| pkg.meta.app == app)
                .ok_or_else(|| format!("no loaded package is the app `{app}`"))?;
            let declared = crate::staging::declared_state(&pkg.meta, app, state)?;
            let value = crate::staging::state_value(&reg, app, &pkg.meta, &declared.ty, text)?;
            (declared.id, value)
        };
        self.core.view_states.borrow_mut().seed(&id, value);
        Ok(())
    }

    /// The workspace's members now, from its membership, and how its grants decide who of
    /// them reads a resource (`None`: everyone reads everything the workspace holds). What
    /// `workspace.members`, `members(r)`, `p.kind()` and `p.role()` read; the host gives them
    /// again whenever membership or grants change, and every view that read them runs again.
    pub fn set_members(
        &self,
        members: Vec<crate::members::Member>,
        reads: Option<Rc<crate::members::Reads>>,
    ) {
        self.core.set_members(members, reads);
    }

    /// Where settled outside effects go.
    pub fn set_sink(&self, sink: impl OutsideSink + 'static) {
        *self.core.sink.borrow_mut() = Some(Box::new(sink));
    }

    /// Outside effects queued and not yet settled.
    pub fn pending_outbox(&self) -> Vec<OutboxEntry> {
        self.core.store.borrow().outbox_pending()
    }

    /// The network came back: entries waiting to go online are tried again on the next settle.
    pub fn retry_waiting(&self) {
        self.core.retry_waiting.set(true);
    }

    /// The earliest moment an outside effect waits to be held until, ISO-8601 UTC text
    /// (`outside(.., at:)`): nothing carries it out before then, and the device sleeps until
    /// it. `None` when none is held back.
    #[must_use]
    pub fn next_held(&self) -> Option<String> {
        self.core.store.borrow().outbox_next_held()
    }

    /// How long from the clock now until the earliest moment an outside effect is held until:
    /// zero once it has come, so the next settle carries it out. `None` when none is held.
    #[must_use]
    pub fn held_for(&self) -> Option<std::time::Duration> {
        let at = self.next_held()?;
        let at = tilolang::calendar::instant(&at)?;
        let now = tilolang::calendar::instant(&self.core.clock_now()).unwrap_or(0);
        Some(std::time::Duration::from_secs(
            u64::try_from(at.saturating_sub(now)).unwrap_or(0),
        ))
    }

    /// Carry the queued outside effects out (convergence.md, "What people see"). An entry is
    /// first marked as being handed over, as a change of its own; the next settle, once that
    /// mark is in the record, hands it to the sink, so a process that stops in between leaves
    /// it as outcome unknown rather than sending it twice. What the sink says settles it (sent,
    /// rejected), keeps it handed over (no answer yet), or has it wait to go online until
    /// [`Self::retry_waiting`]. An entry another process was handing over is outcome unknown.
    ///
    /// # Errors
    ///
    /// `Package` when no sink is set; a store failure.
    pub fn settle(&self) -> Result<SettleReport, RuntimeError> {
        use crate::core::Delivery;
        use crate::store::{OutboxState, Settled};
        let note = |entry: &OutboxEntry, action: &str| ChangeNote {
            action: action.into(),
            args: Json::String(entry.id.clone()),
            invoker: String::new(),
            chain: Vec::new(),
            module: String::new(),
            approved_by: None,
            ..ChangeNote::default()
        };
        let mut report = SettleReport::default();
        let retry = self.core.retry_waiting.replace(false);
        let token = process_token();
        let now = tilolang::calendar::instant(&self.core.clock_now()).unwrap_or(0);
        let pending = self.pending_outbox();
        if !pending.is_empty() && self.core.sink.borrow().is_none() {
            return Err(RuntimeError::Package("no outside sink is set".into()));
        }
        for entry in pending {
            match &entry.state {
                OutboxState::Sending { by } if by != token => {
                    let settle = note(&entry, "$settle");
                    self.core.store.borrow_mut().outbox_settle(
                        &entry,
                        &Settled::Unknown,
                        &settle,
                    )?;
                    report.unknown = report.unknown.saturating_add(1);
                }
                // This process's: handed over, or asked again what became of it. The sink
                // answers for one it already has rather than carrying it out twice.
                OutboxState::Sending { .. } => {
                    let delivered = match self.core.sink.borrow_mut().as_mut() {
                        Some(sink) => sink.deliver(&entry.capability, &entry.payload),
                        None => return Err(RuntimeError::Package("no outside sink is set".into())),
                    };
                    let mut store = self.core.store.borrow_mut();
                    match delivered {
                        // Stays handed over: the sink settles it again once answered
                        // (`Delivery::Pending`).
                        Delivery::Pending => {}
                        Delivery::Delivered => {
                            store.outbox_settle(
                                &entry,
                                &Settled::Done,
                                &note(&entry, "$settle"),
                            )?;
                            report.delivered = report.delivered.saturating_add(1);
                        }
                        Delivery::Rejected(reason) => {
                            let outcome = Settled::Rejected(reason.clone());
                            store.outbox_settle(&entry, &outcome, &note(&entry, "$settle"))?;
                            report.failed.push((entry.id.clone(), reason));
                        }
                        Delivery::Offline(reason) => {
                            let waiting = OutboxState::Waiting { reason };
                            store.outbox_mark(&entry, &waiting, &note(&entry, "$wait"))?;
                            report.waiting = report.waiting.saturating_add(1);
                        }
                    }
                }
                // Resolved by `retry_waiting`, which the origin calls when its service is
                // reachable again.
                OutboxState::Waiting { .. } if !retry => {}
                // Held back until a moment the person's action named (undo send): the device
                // carries it out only once the clock reaches it.
                OutboxState::Queued | OutboxState::Waiting { .. }
                    if entry
                        .held_until
                        .as_deref()
                        .and_then(tilolang::calendar::instant)
                        .is_some_and(|at| at > now) =>
                {
                    report.held = report.held.saturating_add(1);
                }
                OutboxState::Queued | OutboxState::Waiting { .. } => {
                    let sending = OutboxState::Sending {
                        by: token.to_owned(),
                    };
                    self.core.store.borrow_mut().outbox_mark(
                        &entry,
                        &sending,
                        &note(&entry, "$send"),
                    )?;
                    report.marked = report.marked.saturating_add(1);
                }
            }
        }
        self.refresh_deliveries();
        Ok(report)
    }

    /// Bring what `delivery(r)` reads up to date with the outbox: views showing a resource
    /// whose outside effect moved run again.
    pub fn refresh_deliveries(&self) {
        let states = self.core.store.borrow().outbox_states();
        let mut held = self.core.view_states.borrow_mut();
        for (doc, state) in states {
            let key = crate::platform::delivery_key(&doc);
            let value = Value::Tuple(vec![Value::Text(state.state), Value::Text(state.reason)]);
            if held.get(&key) != Some(&value) {
                held.set(key, value);
            }
        }
    }

    /// The notes of a document's changes, oldest first: who changed it, through which code.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing document.
    pub fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, RuntimeError> {
        Ok(self.core.store.borrow().history(doc)?)
    }

    /// The versions of the resource `at`, oldest first ([`Version::order`]): who made each
    /// change that changed it, when, and by which action.
    ///
    /// # Errors
    ///
    /// As [`Runtime::history`].
    pub fn versions(&self, at: &ResourceRef) -> Result<Vec<Version>, RuntimeError> {
        Ok(self.core.store.borrow().versions(at)?)
    }

    /// The modules currently compiled.
    pub fn compiled_modules(&self) -> usize {
        self.core.engine.cached_modules()
    }

    /// This runtime's engine: its instances, its limits and its profile.
    pub fn engine(&self) -> &crate::engine::Engine {
        &self.core.engine
    }

    /// Count what each unit of the packages' code costs from here on: the fuel each call
    /// spends, what it reads of the store, and the offers it publishes, by the unit that ran
    /// ([`crate::cost`]). Cheap while off, which it is until asked: a window opened after this
    /// is what [`Self::costs`] then reports.
    pub fn record_costs(&self, on: bool) {
        self.core.costs.record(on);
        if on {
            self.reset_costs();
        }
    }

    /// Forget what the units cost, keeping [`Self::record_costs`] as it is: what separates one
    /// interaction from the next.
    pub fn reset_costs(&self) {
        let reads = self.core.store.borrow().reads();
        self.core.costs.reset(reads);
    }

    /// What the units cost since [`Self::record_costs`] turned it on or [`Self::reset_costs`]
    /// last cleared it, the one that cost the most fuel first; `None` while nothing is counted.
    #[must_use]
    pub fn costs(&self) -> Option<crate::cost::Costs> {
        self.core
            .costs
            .on()
            .then(|| self.core.costs.costs(self.core.store.borrow().reads()))
    }

    /// How many references to collection items views and code built so far: what reading
    /// collections materialized, a count that does not depend on the machine.
    pub fn items_built(&self) -> u64 {
        self.core.items_built.get()
    }

    /// Hash of a module's bytes, the id [`Runtime::load_package`] returns for it.
    pub fn module_hash(wasm: &[u8]) -> ModuleHash {
        ModuleHash::of_bytes(wasm)
    }
}

/// What reading an action's requirements read of the workspace: the fields, each once, and
/// whether they read more than the workspace holds, which no change in it moves.
#[derive(Clone, Debug, Default)]
pub struct Reading {
    /// The fields the requirements read, by resource and field. A requirement of an item reads
    /// the item's own fields, and a collection it read whole reads every item's under it.
    pub reads: Vec<(String, String)>,
    /// Whether they read the clock, the window's space or the person's view state: the answer
    /// moves without the workspace moving, so it is not kept.
    pub beyond: bool,
}
