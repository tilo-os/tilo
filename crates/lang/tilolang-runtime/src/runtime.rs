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
use crate::store::{ChangeNote, OutboxEntry, Store};
use crate::value::{ActionRef, ResourceRef, Value};
use crate::view_instance::ViewInstance;

/// What settling the outbox did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettleReport {
    pub delivered: usize,
    /// Entries the sink refused, with its message. They stay queued.
    pub failed: Vec<(String, String)>,
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
        let exports = self.core.engine.exports(&hash);
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
        let pkg =
            crate::registry::Package::new(hash.clone(), meta).map_err(RuntimeError::Package)?;
        self.core
            .registry
            .borrow_mut()
            .add(pkg)
            .map_err(RuntimeError::Package)?;
        Ok(hash)
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

    /// Open a view for `principal`.
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
        ViewInstance::open(Rc::clone(&self.core), principal.clone(), view, args)
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
