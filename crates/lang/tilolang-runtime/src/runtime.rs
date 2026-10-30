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
use crate::flow::{FlowOutcome, Resolution};
use crate::ids::{DocId, ModuleHash, PackageId, Principal, RunId};
use crate::meta::Meta;
use crate::project::{ProjectOptions, project};
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

/// The tilolang runtime: loads packages and runs their actions, flows and views.
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
        Ok(Self { core })
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
        self.check_exports(&hash, &meta)?;
        let pkg =
            crate::registry::Package::new(hash.clone(), meta).map_err(RuntimeError::Package)?;
        self.core
            .registry
            .borrow_mut()
            .add(pkg)
            .map_err(RuntimeError::Package)?;
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
        self.check_exports(&hash, &meta)?;
        let pkg =
            crate::registry::Package::new(hash.clone(), meta).map_err(RuntimeError::Package)?;
        self.core
            .registry
            .borrow_mut()
            .replace(pkg)
            .map_err(RuntimeError::Package)?;
        Ok(hash)
    }

    fn check_exports(&self, hash: &ModuleHash, meta: &Meta) -> Result<(), RuntimeError> {
        let exports = self.core.engine.exports(hash);
        let wanted = meta
            .actions
            .iter()
            .map(|a| format!("a_{}", a.id))
            .chain(meta.views.iter().map(|v| format!("v_{}", v.id)))
            .chain(meta.flows.iter().map(|f| format!("f_{}", f.id)))
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
            }));
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
    ///
    /// # Errors
    /// Its diagnostics, located in `source`.
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
        suggestion: &crate::flow::Suggestion,
    ) -> Result<ResourceRef, RuntimeError> {
        let aref = ActionRef {
            action: action.to_owned(),
            args,
            author: None,
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

    /// Create the per-room root of a type, by id or by a name one loaded package declares: the
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

    /// Read a field of a resource, by field id or name.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown type or field; a store failure.
    pub fn read(&self, at: &ResourceRef, field: &str) -> Result<Value, RuntimeError> {
        let (_, info) = self
            .core
            .registry
            .borrow()
            .type_info(&at.ty)
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", at.ty)))?;
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

    /// Invoke an action as `principal`. The action is an [`ActionRef`], or an
    /// `(action id, args)` pair for a call made straight by the principal.
    ///
    /// # Errors
    ///
    /// An unknown action, arguments that do not fit, or a trap (rolled back).
    pub fn invoke_action(
        &self,
        principal: &Principal,
        action: impl Into<ActionRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.core.invoke_direct(principal, &action.into())
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

    /// The default of the parameter `at` of `action`, computed now over the arguments before
    /// it: what its form starts filled in with. `None` when it has none.
    ///
    /// # Errors
    ///
    /// A default that traps.
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

    /// Run a flow. It finishes, or suspends on a suggestion waiting for a decision.
    ///
    /// # Errors
    ///
    /// An unknown flow, arguments that do not fit, or a trap.
    pub fn run_flow(
        &self,
        principal: &Principal,
        flow: &str,
        args: Vec<Value>,
    ) -> Result<FlowOutcome, RuntimeError> {
        self.core.start_flow(principal, flow, args, Vec::new())
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
    ) -> Result<FlowOutcome, RuntimeError> {
        self.core.resolve(run, approver, resolution)
    }

    /// Open a view for `principal`, in the window of its first resource argument.
    ///
    /// # Errors
    ///
    /// An unknown view, arguments that do not fit, or a trap.
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

    /// Open a view for `principal` in the window `window`: its `window` state is the one every
    /// view in that window shares, its `lens` state the principal's own there.
    ///
    /// # Errors
    ///
    /// An unknown view, arguments that do not fit, or a trap.
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

    /// Signatures of every loaded package, for discovery ([`catalogue`](mod@crate::catalogue)).
    pub fn catalogue(&self) -> Vec<Signature> {
        let pkgs = self.core.registry.borrow().packages();
        let metas: Vec<&Meta> = pkgs.iter().map(|p| &p.meta).collect();
        catalogue(&metas)
    }

    /// Where `model_call` goes.
    pub fn set_model_host(&self, host: impl ModelHost + 'static) {
        *self.core.models.borrow_mut() = Some(Box::new(host));
    }

    /// How views show a principal's name. Until it is set, a principal is its id.
    pub fn set_name_host(&self, host: impl crate::core::NameHost + 'static) {
        *self.core.names.borrow_mut() = Some(Box::new(host));
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

    /// What `phrase(id, ...)` reads: the platform's phrases in the person's language. Until
    /// it is set, a phrase is its id.
    pub fn set_phrases(&self, phrases: impl Fn(&str, &[(String, String)]) -> String + 'static) {
        *self.core.phrases.borrow_mut() = Some(Box::new(phrases));
    }

    /// What a reference to a resource the person cannot read says, in their language: the
    /// chip's wording (`tilo_reference::embed::UNSHARED`).
    #[must_use]
    pub fn unshared(&self) -> String {
        self.core.unshared()
    }

    /// The lenses on the resource `resource` (its id) now, as presence reports them: who, and
    /// whether it is the person looking. What `lenses(r)` reads; a view that read the lenses
    /// on it runs again when they move.
    pub fn set_lenses(&self, resource: &str, lenses: &[(Principal, bool)]) {
        let key = crate::platform::lens_key(resource);
        let value = Value::List(
            lenses
                .iter()
                .map(|(who, mine)| {
                    Value::Tuple(vec![Value::Principal(who.clone()), Value::Bool(*mine)])
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

    /// The task runs `person` reviewed, as their own record keeps them: every version of one is
    /// seen. A view that read what is unseen runs again when they move.
    pub fn set_seen_runs(&self, person: &str, runs: &[String]) {
        self.core.set_seen_runs(person, runs);
    }

    /// `person` reviewed the task run `run` (its group, *Mark all seen*): every version it made
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

    /// The seen marks set here since last asked, for the host to keep in the person's own
    /// record.
    pub fn take_seen(&self) -> Vec<crate::seen::SeenWrite> {
        std::mem::take(&mut *self.core.seen_writes.borrow_mut())
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

    /// The modules currently compiled.
    pub fn compiled_modules(&self) -> usize {
        self.core.engine.cached_modules()
    }

    /// This runtime's engine: its instances, its limits and its profile.
    pub fn engine(&self) -> &crate::engine::Engine {
        &self.core.engine
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
