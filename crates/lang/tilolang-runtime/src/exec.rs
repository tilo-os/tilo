//! Invocation lifecycles (implementation design 4.5): actions, flows and views.

use std::collections::HashSet;

use serde_json::Value as Json;

use crate::abi::Imp;
use crate::authority::{ActionInfo, Decision};
use crate::core::{Args, Core, HResult, trap};
use crate::engine::{CallError, Host, HostFail, Wasm};
use crate::error::RuntimeError;
use crate::flow::{FlowOutcome, Journal, JournalEntry, Resolution, Suggestion};
use crate::handles::{ActionState, FlowState, FrameKind, Retained, ViewState};
use crate::ids::{ModuleHash, Principal, RunId};
use crate::meta::{EffectClass, ParamMeta, Repr, Ty};
use crate::registry::Package;
use crate::space::Space;
use crate::store::{ChangeNote, Mutation};
use crate::value::{ActionRef, Value, to_json};
use crate::view::{NodeBuild, UiNode};

/// Who an invocation runs for, and through which code.
#[derive(Clone, Debug)]
pub struct ExecCtx {
    pub invoker: Principal,
    pub chain: Vec<ModuleHash>,
    pub approved_by: Option<Principal>,
}

/// How an action ended.
#[derive(Debug)]
pub enum ActionOutcome {
    Done(Value),
    /// The action trapped or ran out of fuel; its change was rolled back.
    Failed(String),
}

/// How a direct invocation ended.
#[derive(Clone, Debug, PartialEq)]
pub enum InvokeOutcome {
    Done(Value),
    Suggested(Suggestion),
    Denied,
}

/// A rendered view: its tree, what it read, and the action references it retained.
pub struct Rendered {
    pub root: UiNode,
    pub reads: Vec<(String, String)>,
    pub space_reads: Vec<(String, f64)>,
    pub retained: Vec<Retained>,
}

enum Step {
    Replay(Value),
    Live(ExecCtx),
}

fn push_chain(chain: &mut Vec<ModuleHash>, author: Option<&ModuleHash>) {
    if let Some(a) = author
        && chain.last() != Some(a)
    {
        chain.push(a.clone());
    }
}

impl Core {
    // ----- marshalling -------------------------------------------------------------------

    fn check_param(&self, pkg: &Package, ty: &Ty, v: &Value) -> Result<(), String> {
        let ok = match (ty, v) {
            (Ty::Named(name), Value::Ref(r)) => {
                let want = self.registry.borrow().resolve_type(pkg, name);
                want.as_deref() == Some(r.target.ty.as_str())
            }
            (Ty::Text, Value::Text(_))
            | (Ty::List(_) | Ty::Set(_) | Ty::Map(_) | Ty::Collection(_), Value::List(_))
            | (Ty::Option(_), Value::Opt(_))
            | (Ty::Tuple(_), Value::Tuple(_))
            | (Ty::Action, Value::Action(_))
            | (Ty::View, _) => true,
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!(
                "a {} does not fit a parameter of type {ty:?}",
                v.kind_name()
            ))
        }
    }

    /// Turn arguments into wasm values in the current frame.
    fn marshal(
        &self,
        pkg: &Package,
        params: &[ParamMeta],
        args: &[Value],
    ) -> Result<Vec<Wasm>, String> {
        if params.len() != args.len() {
            return Err(format!(
                "expected {} arguments, got {}",
                params.len(),
                args.len()
            ));
        }
        params
            .iter()
            .zip(args)
            .map(|(p, v)| {
                let ty = Ty::parse(&p.ty)?;
                let mismatch = || {
                    format!(
                        "argument `{}`: a {} is not a {}",
                        p.name,
                        v.kind_name(),
                        p.ty
                    )
                };
                match ty.repr() {
                    Repr::I64 => v.as_int().map(Wasm::I64).ok_or_else(mismatch),
                    Repr::F64 => match v {
                        Value::Float(f) => Ok(Wasm::F64(*f)),
                        _ => Err(mismatch()),
                    },
                    Repr::Bool => v
                        .as_int()
                        .map(|i| Wasm::I32(i32::from(i != 0)))
                        .ok_or_else(mismatch),
                    Repr::Handle => {
                        self.check_param(pkg, &ty, v)
                            .map_err(|e| format!("argument `{}`: {e}", p.name))?;
                        self.frames.borrow_mut().alloc(v.clone()).map(Wasm::I32)
                    }
                }
            })
            .collect()
    }

    fn unmarshal(&self, result: Option<&str>, w: Option<Wasm>) -> Result<Value, String> {
        let Some(ty) = result else {
            return Ok(Value::Unit);
        };
        let ty = Ty::parse(ty)?;
        match (ty.repr(), w) {
            (Repr::I64, Some(Wasm::I64(i))) => Ok(Value::Int(i)),
            (Repr::F64, Some(Wasm::F64(f))) => Ok(Value::Float(f)),
            (Repr::Bool, Some(Wasm::I32(i))) => Ok(Value::Bool(i != 0)),
            (Repr::Handle, Some(Wasm::I32(h))) => Ok(self.frames.borrow().get(h)?.clone()),
            _ => Err("the export returned a value of the wrong type".to_owned()),
        }
    }

    /// Marshal, call and unmarshal in the frame that is already pushed.
    fn call_in_frame(
        &self,
        pkg: &Package,
        export: &str,
        params: &[ParamMeta],
        args: &[Value],
        result: Option<&str>,
    ) -> Result<Value, RuntimeError> {
        let wasm_args = self
            .marshal(pkg, params, args)
            .map_err(RuntimeError::BadArgs)?;
        let host: std::rc::Rc<dyn Host> = self.rc().map_err(RuntimeError::Package)?;
        let out = self.engine.call(&pkg.hash, export, &wasm_args, host)?;
        self.unmarshal(result, out).map_err(RuntimeError::Package)
    }

    // ----- authority ---------------------------------------------------------------------

    fn decide(&self, ctx: &ExecCtx, aref: &ActionRef) -> Result<Decision, RuntimeError> {
        let pkg = self
            .registry
            .borrow()
            .action_package(&aref.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", aref.action)))?;
        let meta = pkg
            .action(&aref.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", aref.action)))?;
        let info = ActionInfo {
            action: aref.action.clone(),
            app: pkg.meta.app.clone(),
            class: meta.class,
            own: aref.author.as_ref().is_none_or(|a| *a == pkg.hash),
            module: pkg.hash.clone(),
        };
        Ok(self.authority.decide(&ctx.invoker, &ctx.chain, &info))
    }

    pub(crate) fn decide_with(&self, ctx: &ExecCtx, info: &ActionInfo) -> Decision {
        self.authority.decide(&ctx.invoker, &ctx.chain, info)
    }

    // ----- actions -----------------------------------------------------------------------

    fn note(ctx: &ExecCtx, pkg: &Package, aref: &ActionRef) -> ChangeNote {
        ChangeNote {
            action: aref.action.clone(),
            args: Json::Array(
                aref.args
                    .iter()
                    .map(|v| to_json(v).unwrap_or(Json::Null))
                    .collect(),
            ),
            invoker: ctx.invoker.0.clone(),
            chain: ctx.chain.iter().map(|m| m.0.clone()).collect(),
            module: pkg.hash.0.clone(),
            approved_by: ctx.approved_by.as_ref().map(|p| p.0.clone()),
        }
    }

    /// Run an action in a transaction: one change per touched document, or nothing.
    ///
    /// # Errors
    ///
    /// An unknown action, arguments that do not fit, or a store failure. A trap is an
    /// `Ok(ActionOutcome::Failed)`: the action was rolled back as designed.
    pub fn run_action(
        &self,
        ctx: &ExecCtx,
        aref: &ActionRef,
    ) -> Result<ActionOutcome, RuntimeError> {
        let pkg = self
            .registry
            .borrow()
            .action_package(&aref.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", aref.action)))?;
        let meta = pkg
            .action(&aref.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", aref.action)))?
            .clone();
        let txn = self.store.borrow_mut().begin()?;
        let primary = aref.args.iter().find_map(|v| match v {
            Value::Ref(r) => Some(r.target.clone()),
            _ => None,
        });
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::Action(ActionState {
                txn,
                action: aref.action.clone(),
                primary,
            }),
        );
        let result = self.call_in_frame(
            &pkg,
            &format!("a_{}", aref.action),
            &meta.params,
            &aref.args,
            meta.result.as_deref(),
        );
        self.frames.borrow_mut().pop();
        match result {
            Ok(v) => {
                let note = Self::note(ctx, &pkg, aref);
                self.store.borrow_mut().commit(txn, &note)?;
                Ok(ActionOutcome::Done(v))
            }
            Err(RuntimeError::BadArgs(m)) => {
                self.store.borrow_mut().rollback(txn);
                Err(RuntimeError::BadArgs(m))
            }
            Err(e) => {
                self.store.borrow_mut().rollback(txn);
                Ok(ActionOutcome::Failed(e.to_string()))
            }
        }
    }

    /// A direct invocation by a principal, or through an action reference some code wrote.
    ///
    /// # Errors
    ///
    /// As [`Core::run_action`]; a trap is an error here since nobody journals it.
    pub fn invoke_direct(
        &self,
        invoker: &Principal,
        aref: &ActionRef,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let mut chain = Vec::new();
        push_chain(&mut chain, aref.author.as_ref());
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            chain,
            approved_by: None,
        };
        match self.decide(&ctx, aref)? {
            Decision::Allow => match self.run_action(&ctx, aref)? {
                ActionOutcome::Done(v) => Ok(InvokeOutcome::Done(v)),
                ActionOutcome::Failed(m) => Err(RuntimeError::Trap(m)),
            },
            Decision::Deny => Ok(InvokeOutcome::Denied),
            Decision::Ask => {
                let run = self.store.borrow_mut().fresh_id("s");
                let suggestion = Suggestion {
                    run: run.clone(),
                    action: aref.action.clone(),
                    args: aref.args.clone(),
                    invoker: ctx.invoker.clone(),
                    chain: ctx.chain.clone(),
                };
                let journal = Journal {
                    flow: None,
                    args: aref.args.clone(),
                    invoker: ctx.invoker,
                    chain: ctx.chain,
                    entries: Vec::new(),
                    pending: Some(suggestion.clone()),
                    finished: false,
                };
                self.store.borrow_mut().save_journal(&run, &journal)?;
                Ok(InvokeOutcome::Suggested(suggestion))
            }
        }
    }

    // ----- flows -------------------------------------------------------------------------

    /// Start a flow run.
    ///
    /// # Errors
    ///
    /// An unknown flow, arguments that do not fit, a trap, or a store failure.
    pub fn start_flow(
        &self,
        invoker: &Principal,
        flow: &str,
        args: Vec<Value>,
        chain: Vec<ModuleHash>,
    ) -> Result<FlowOutcome, RuntimeError> {
        if self.registry.borrow().flow_package(flow).is_none() {
            return Err(RuntimeError::NotFound(format!("flow {flow}")));
        }
        let run = self.store.borrow_mut().fresh_id("r");
        let journal = Journal {
            flow: Some(flow.to_owned()),
            args,
            invoker: invoker.clone(),
            chain,
            entries: Vec::new(),
            pending: None,
            finished: false,
        };
        self.store.borrow_mut().save_journal(&run, &journal)?;
        self.run_journal(&run, journal)
    }

    /// Run a flow from the start, answering calls from its journal while it lasts.
    fn run_journal(&self, run: &RunId, journal: Journal) -> Result<FlowOutcome, RuntimeError> {
        let flow_id = journal
            .flow
            .clone()
            .ok_or_else(|| RuntimeError::NotFound("a flow for this run".into()))?;
        let pkg = self
            .registry
            .borrow()
            .flow_package(&flow_id)
            .ok_or_else(|| RuntimeError::NotFound(format!("flow {flow_id}")))?;
        let meta = pkg
            .flow(&flow_id)
            .ok_or_else(|| RuntimeError::NotFound(format!("flow {flow_id}")))?
            .clone();
        let args = journal.args.clone();
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::Flow(Box::new(FlowState {
                run: run.clone(),
                invoker: journal.invoker.clone(),
                journal,
                cursor: 0,
            })),
        );
        let result = self.call_in_frame(
            &pkg,
            &format!("f_{flow_id}"),
            &meta.params,
            &args,
            meta.result.as_deref(),
        );
        let frame = self.frames.borrow_mut().pop();
        let Some(FrameKind::Flow(state)) = frame.map(|f| f.kind) else {
            return Err(RuntimeError::Package("the flow frame vanished".into()));
        };
        let mut journal = state.journal;
        match result {
            Ok(v) => {
                journal.finished = true;
                journal.pending = None;
                self.store.borrow_mut().save_journal(run, &journal)?;
                Ok(FlowOutcome::Done(v))
            }
            Err(RuntimeError::Engine(CallError::Suspended)) => {
                self.store.borrow_mut().save_journal(run, &journal)?;
                journal.pending.map(FlowOutcome::Suspended).ok_or_else(|| {
                    RuntimeError::Package("the flow suspended without a suggestion".into())
                })
            }
            Err(e) => {
                self.store.borrow_mut().save_journal(run, &journal)?;
                Err(e)
            }
        }
    }

    /// Answer a suggestion. Approving runs the suggested action with the approver recorded;
    /// declining journals a refusal. A flow then replays from its journal.
    ///
    /// # Errors
    ///
    /// `NotPending` when nothing waits on this run.
    pub fn resolve(
        &self,
        run: &RunId,
        approver: &Principal,
        resolution: Resolution,
    ) -> Result<FlowOutcome, RuntimeError> {
        let mut journal = self
            .store
            .borrow_mut()
            .load_journal(run)?
            .ok_or_else(|| RuntimeError::NotFound(format!("run {run}")))?;
        let pending = match journal.pending.take() {
            Some(p) if !journal.finished => p,
            _ => return Err(RuntimeError::NotPending(run.clone())),
        };
        let entry = match resolution {
            Resolution::Approve => {
                let ctx = ExecCtx {
                    invoker: pending.invoker.clone(),
                    chain: pending.chain.clone(),
                    approved_by: Some(approver.clone()),
                };
                let aref = ActionRef::new(pending.action.clone(), pending.args.clone());
                match self.run_action(&ctx, &aref)? {
                    ActionOutcome::Done(result) => JournalEntry::Invoked {
                        action: pending.action,
                        result,
                    },
                    ActionOutcome::Failed(reason) => JournalEntry::Failed {
                        action: pending.action,
                        reason,
                    },
                }
            }
            Resolution::Decline => JournalEntry::Denied {
                action: pending.action,
                reason: format!("declined by {approver}"),
            },
        };
        journal.entries.push(entry.clone());
        if journal.flow.is_none() {
            journal.finished = true;
            self.store.borrow_mut().save_journal(run, &journal)?;
            return Ok(FlowOutcome::Done(entry_value(&entry)));
        }
        self.store.borrow_mut().save_journal(run, &journal)?;
        self.run_journal(run, journal)
    }

    fn journal_push(&self, entry: JournalEntry) -> HResult<()> {
        let (run, journal) = {
            let mut frames = self.frames.borrow_mut();
            let FrameKind::Flow(fs) = &mut frames.top_mut()?.kind else {
                return trap("not a flow frame");
            };
            fs.journal.entries.push(entry);
            fs.cursor = fs.cursor.saturating_add(1);
            (fs.run.clone(), fs.journal.clone())
        };
        self.store
            .borrow_mut()
            .save_journal(&run, &journal)
            .map_err(|e| HostFail::Trap(e.to_string()))
    }

    fn flow_invoke(&self, h: i32) -> HResult<Option<Wasm>> {
        let Value::Action(aref) = self.val(h)? else {
            return trap("expected an action reference");
        };
        let step = {
            let mut frames = self.frames.borrow_mut();
            let FrameKind::Flow(fs) = &mut frames.top_mut()?.kind else {
                return trap("invoke is only available in flows");
            };
            if let Some(entry) = fs.journal.entries.get(fs.cursor) {
                let v = replay_value(entry, &aref.action)?;
                fs.cursor = fs.cursor.saturating_add(1);
                Step::Replay(v)
            } else {
                let mut chain = fs.journal.chain.clone();
                push_chain(&mut chain, aref.author.as_ref());
                Step::Live(ExecCtx {
                    invoker: fs.invoker.clone(),
                    chain,
                    approved_by: None,
                })
            }
        };
        let ctx = match step {
            Step::Replay(v) => return self.ret_h(v),
            Step::Live(ctx) => ctx,
        };
        let decision = self
            .decide(&ctx, &aref)
            .map_err(|e| HostFail::Trap(e.to_string()))?;
        match decision {
            Decision::Allow => {
                let outcome = self
                    .run_action(&ctx, &aref)
                    .map_err(|e| HostFail::Trap(e.to_string()))?;
                let (entry, value) = match outcome {
                    ActionOutcome::Done(result) => (
                        JournalEntry::Invoked {
                            action: aref.action,
                            result: result.clone(),
                        },
                        result,
                    ),
                    ActionOutcome::Failed(reason) => (
                        JournalEntry::Failed {
                            action: aref.action,
                            reason: reason.clone(),
                        },
                        Value::Error(reason),
                    ),
                };
                self.journal_push(entry)?;
                self.ret_h(value)
            }
            Decision::Deny => {
                let reason = "denied by authority".to_owned();
                self.journal_push(JournalEntry::Denied {
                    action: aref.action,
                    reason: reason.clone(),
                })?;
                self.ret_h(Value::Error(reason))
            }
            Decision::Ask => {
                self.suspend_on(&ctx, &aref)?;
                Err(HostFail::Suspend)
            }
        }
    }

    /// Record the pending suggestion in the flow's journal and persist it.
    fn suspend_on(&self, ctx: &ExecCtx, aref: &ActionRef) -> HResult<()> {
        let (run, journal) = {
            let mut frames = self.frames.borrow_mut();
            let FrameKind::Flow(fs) = &mut frames.top_mut()?.kind else {
                return trap("not a flow frame");
            };
            fs.journal.pending = Some(Suggestion {
                run: fs.run.clone(),
                action: aref.action.clone(),
                args: aref.args.clone(),
                invoker: ctx.invoker.clone(),
                chain: ctx.chain.clone(),
            });
            (fs.run.clone(), fs.journal.clone())
        };
        self.store
            .borrow_mut()
            .save_journal(&run, &journal)
            .map_err(|e| HostFail::Trap(e.to_string()))
    }

    fn model_call(&self, kind_h: i32, input_h: i32) -> HResult<Option<Wasm>> {
        let (kind, input) = (self.text(kind_h)?, self.val(input_h)?);
        {
            let mut frames = self.frames.borrow_mut();
            let FrameKind::Flow(fs) = &mut frames.top_mut()?.kind else {
                return trap("model_call is only available in flows");
            };
            if let Some(entry) = fs.journal.entries.get(fs.cursor) {
                let JournalEntry::Model { kind: k, result } = entry else {
                    return trap("the flow diverged from its journal: expected a model call");
                };
                if *k != kind {
                    return trap(format!(
                        "the flow diverged from its journal: model call `{k}`"
                    ));
                }
                let result = result.clone();
                fs.cursor = fs.cursor.saturating_add(1);
                drop(frames);
                return self.ret_h(result);
            }
        }
        let answer = match self.models.borrow().as_ref() {
            Some(m) => m.call(&kind, &input),
            None => Err("no model provider is configured".to_owned()),
        }?;
        self.journal_push(JournalEntry::Model {
            kind,
            result: answer.clone(),
        })?;
        self.ret_h(answer)
    }

    // ----- effects imports ---------------------------------------------------------------

    fn outside(&self, cap_h: i32, payload_h: i32) -> HResult<Option<Wasm>> {
        let capability = self.text(cap_h)?;
        let payload = to_json(&self.val(payload_h)?)?;
        if capability != "post" {
            return trap(format!("unknown outside capability `{capability}`"));
        }
        let (txn, action, primary) = {
            let frames = self.frames.borrow();
            match &frames.top()?.kind {
                FrameKind::Action(a) => (a.txn, a.action.clone(), a.primary.clone()),
                _ => return trap("outside is only available in actions"),
            }
        };
        let class = self
            .registry
            .borrow()
            .action_package(&action)
            .and_then(|p| p.action(&action).map(|a| a.class));
        if class != Some(EffectClass::Outside) {
            return trap(format!("action `{action}` is not classed outside"));
        }
        let Some(primary) = primary else {
            return trap("an outside action needs a resource argument to queue its effect on");
        };
        self.store
            .borrow_mut()
            .apply(
                txn,
                &primary,
                &Mutation::OutboxPush {
                    capability,
                    payload,
                },
            )
            .map_err(|e| HostFail::Trap(e.to_string()))?;
        self.ret_h(Value::Unit)
    }

    fn act_ref(&self, id_h: i32, args_h: i32) -> HResult<Option<Wasm>> {
        let action = self.text(id_h)?;
        let Value::List(args) = self.val(args_h)? else {
            return trap("act_ref expects the arguments as a list");
        };
        let arity = self
            .registry
            .borrow()
            .action_package(&action)
            .and_then(|p| p.action(&action).map(|a| a.params.len()));
        match arity {
            None => return trap(format!("unknown action `{action}`")),
            Some(n) if n != args.len() => {
                return trap(format!(
                    "action `{action}` takes {n} arguments, got {}",
                    args.len()
                ));
            }
            Some(_) => {}
        }
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Action(ActionRef {
            action,
            args,
            author,
        }))
    }

    pub(crate) fn effect_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::ActRef => self.act_ref(a.h(0)?, a.h(1)?),
            Imp::Invoke => self.flow_invoke(a.h(0)?),
            Imp::Outside => self.outside(a.h(0)?, a.h(1)?),
            Imp::ModelCall => self.model_call(a.h(0)?, a.h(1)?),
            _ => trap("not an effect import"),
        }
    }

    // ----- views -------------------------------------------------------------------------

    /// Run a view and build its tree.
    ///
    /// # Errors
    ///
    /// An unknown view, arguments that do not fit, a trap, or a malformed tree.
    pub fn render(
        &self,
        view: &str,
        args: &[Value],
        space: Space,
    ) -> Result<Rendered, RuntimeError> {
        let pkg = self
            .registry
            .borrow()
            .view_package(view)
            .ok_or_else(|| RuntimeError::NotFound(format!("view {view}")))?;
        let meta = pkg
            .view(view)
            .ok_or_else(|| RuntimeError::NotFound(format!("view {view}")))?
            .clone();
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::View(ViewState {
                space,
                ..ViewState::default()
            }),
        );
        let root = self.call_in_frame(&pkg, &format!("v_{view}"), &meta.params, args, Some("View"));
        let frame = self.frames.borrow_mut().pop();
        let Some(FrameKind::View(state)) = frame.map(|f| f.kind) else {
            return Err(RuntimeError::Package("the view frame vanished".into()));
        };
        let Value::Node(root) = root? else {
            return Err(RuntimeError::Package("a view must return a ui node".into()));
        };
        let mut retained = Vec::new();
        let tree = self.build_node(&state.nodes, root, 0, &mut retained)?;
        Ok(Rendered {
            root: tree,
            reads: state.reads,
            space_reads: state.space_reads,
            retained,
        })
    }

    fn build_node(
        &self,
        nodes: &[NodeBuild],
        at: usize,
        depth: usize,
        retained: &mut Vec<Retained>,
    ) -> Result<UiNode, RuntimeError> {
        if depth > nodes.len() {
            return Err(RuntimeError::Package("the ui tree contains a cycle".into()));
        }
        let n = nodes
            .get(at)
            .ok_or_else(|| RuntimeError::Package("unknown ui node".into()))?;
        let mut keys = HashSet::new();
        let mut children = Vec::new();
        for c in &n.children {
            let child = self.build_node(nodes, *c, depth.saturating_add(1), retained)?;
            if !keys.insert(child.key.clone()) {
                return Err(RuntimeError::Package(format!(
                    "duplicate key `{}` among the children of `{}`",
                    child.key, n.key
                )));
            }
            children.push(child);
        }
        let events = n
            .events
            .iter()
            .map(|(name, v)| {
                let r = self.frames.borrow_mut().retain(v.clone());
                retained.push(r);
                (name.clone(), r)
            })
            .collect();
        Ok(UiNode {
            kind: n.kind.clone(),
            key: n.key.clone(),
            props: n.props.clone(),
            binding: n.binding.clone(),
            events,
            anchor: n.anchor.clone(),
            children,
        })
    }
}

fn entry_value(entry: &JournalEntry) -> Value {
    match entry {
        JournalEntry::Invoked { result, .. } | JournalEntry::Model { result, .. } => result.clone(),
        JournalEntry::Denied { reason, .. } | JournalEntry::Failed { reason, .. } => {
            Value::Error(reason.clone())
        }
    }
}

fn replay_value(entry: &JournalEntry, action: &str) -> HResult<Value> {
    match entry {
        JournalEntry::Invoked { action: a, .. }
        | JournalEntry::Denied { action: a, .. }
        | JournalEntry::Failed { action: a, .. }
            if a == action =>
        {
            Ok(entry_value(entry))
        }
        _ => trap(format!(
            "the flow diverged from its journal: it now invokes `{action}`"
        )),
    }
}
