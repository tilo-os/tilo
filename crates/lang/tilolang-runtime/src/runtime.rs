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
use crate::store::{ChangeNote, OutboxEntry, Store};
use crate::value::{ActionRef, ResourceRef, Value};
use crate::view_instance::ViewInstance;
use tilolang::{BlockKind, Compiled, CompiledBlock};

/// What settling the outbox did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettleReport {
    pub delivered: usize,
    /// Entries the sink refused, with its message. They stay queued.
    pub failed: Vec<(String, String)>,
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
    fn deliver(&mut self, capability: &str, payload: &Json) -> Result<(), String> {
        self.delivered
            .borrow_mut()
            .push((capability.to_owned(), payload.clone()));
        Ok(())
    }
}

/// The tilolang runtime: loads packages and runs their actions, flows and views.
pub struct Runtime {
    core: Rc<Core>,
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
            .chain(meta.fns.iter().map(|f| format!("n_{}", f.id)));
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

    /// Evaluate an agent block (implementation design 6.1) as `principal`. A view renders and
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
    /// `Invocation` resource (implementation design 6.1, 7.4).
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

    /// What an agent sees of a value (implementation design 6.2).
    pub fn project(&self, value: &Value) -> Json {
        project(&self.core, value, ProjectOptions::default())
    }

    /// As [`Runtime::project`] with explicit limits.
    pub fn project_with(&self, value: &Value, opts: ProjectOptions) -> Json {
        project(&self.core, value, opts)
    }

    /// Signatures of every loaded package, for discovery (implementation design 6.3).
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

    /// Where settled outside effects go.
    pub fn set_sink(&self, sink: impl OutsideSink + 'static) {
        *self.core.sink.borrow_mut() = Some(Box::new(sink));
    }

    /// Outside effects queued and not yet settled.
    pub fn pending_outbox(&self) -> Vec<OutboxEntry> {
        self.core.store.borrow().outbox_pending()
    }

    /// Deliver every queued outside effect to the sink and mark it settled.
    ///
    /// # Errors
    ///
    /// `Package` when no sink is set; a store failure.
    pub fn settle(&self) -> Result<SettleReport, RuntimeError> {
        let mut report = SettleReport::default();
        for entry in self.pending_outbox() {
            let delivered = match self.core.sink.borrow_mut().as_mut() {
                Some(sink) => sink.deliver(&entry.capability, &entry.payload),
                None => return Err(RuntimeError::Package("no outside sink is set".into())),
            };
            match delivered {
                Ok(()) => {
                    let note = ChangeNote {
                        action: "$settle".into(),
                        args: Json::String(entry.id.clone()),
                        invoker: String::new(),
                        chain: Vec::new(),
                        module: String::new(),
                        approved_by: None,
                    };
                    self.core.store.borrow_mut().outbox_settle(&entry, &note)?;
                    report.delivered = report.delivered.saturating_add(1);
                }
                Err(msg) => report.failed.push((entry.id, msg)),
            }
        }
        Ok(report)
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

    /// Hash of a module's bytes, the id [`Runtime::load_package`] returns for it.
    pub fn module_hash(wasm: &[u8]) -> ModuleHash {
        ModuleHash::of_bytes(wasm)
    }
}
