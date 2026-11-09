//! Invocation lifecycles.
//!
//! * **An action** runs in one store transaction. Its writes go into the transaction; if it
//!   returns, the store commits one change per touched document, each carrying a
//!   [`ChangeNote`] (the action, its arguments, the invoker, the authorship chain and the
//!   module). If it traps or runs out of fuel, nothing is committed.
//! * **An action that calls another action** is a run: a journal ([`crate::run`]). A call goes
//!   through the door: the authority decides it, and it runs as a change of its own. The
//!   caller's transaction ends at the call, and it runs again from its journal, which answers
//!   the call and everything it read before it. An invocation the authority or a person must
//!   answer first suspends the run; the answer runs it on from the same journal, so nothing runs
//!   twice.
//! * **An agent's program** is the same run without a transaction: every change it makes goes
//!   through the host's invoker, to the app that holds the records.
//!
//! An action's arguments left out are completed from their defaults, on the invoker's state,
//! before it is decided, journaled or run. An action's `outside` call doesn't reach outside
//! while the action runs: it is queued in the action's change as an outbox entry, which the
//! device that holds the capability settles.

use std::rc::Rc;

use serde_json::Value as Json;

use crate::abi::Imp;
use crate::authority::{ActionInfo, Decision};
use crate::core::{Args, Core, HResult, ret_i32, ret_void, trap};
use crate::engine::{CallError, Host, HostFail, Wasm};
use crate::error::RuntimeError;
use crate::handles::{ActionState, FrameKind, Invocation, QueuedBlock, RunState};
use crate::host_res::unboxed;
use crate::ids::{ModuleHash, Principal, RunId};
use crate::meta::{ActionMeta, EffectClass, ParamMeta, Repr, Ty, parse_ty, repr};
use crate::registry::Package;
use crate::run::{Journal, JournalEntry, Pending, Resolution, RunOutcome, Step, Suggestion};
use crate::store::{Applied, ChangeNote, Mutation, Txn};
use crate::value::{ActionRef, ResourceRef, Value, to_json};

/// Calls into other packages' `fn`s deeper than this are refused: each one is a fresh instance
/// of the callee's module, running on the host's stack, so recursion across packages would
/// otherwise exhaust it.
const MAX_CALL_DEPTH: usize = 32;

/// Who an invocation runs for, and through which code.
#[derive(Clone, Debug)]
pub struct ExecCtx {
    pub invoker: Principal,
    pub chain: Vec<ModuleHash>,
    pub approved_by: Option<Principal>,
    /// The run this invocation already is, when something every replica reads names it: the
    /// record of a suggestion names the run that waits on a person (`InvocationRecord::run`),
    /// and a device that holds no journal of it runs its action under that run. Every effect the
    /// run queues is keyed by the id it starts with, so a device that runs the same invocation
    /// queues each effect once. Without one, the run gets an id of its own.
    pub identity: Option<RunId>,
    /// The resource the invocation was made from, when it was made from one: what the resources
    /// it creates were made from, recorded in the change ([`crate::store::Version::from`]).
    pub from: Option<ResourceRef>,
    /// The moment the invocation acts at, when a scenario named one: what its clock reads.
    pub at: Option<String>,
    /// Who proposed the action, when the person invoking it accepted a suggestion: recorded in
    /// its change, so history says who suggested it.
    pub suggested_by: Option<Principal>,
}

/// How an action ended.
#[derive(Debug)]
pub enum ActionOutcome {
    Done(Value),
    /// It invoked an action a person must answer first: the run waits for them.
    Suspended(Suggestion),
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

/// What one attempt at a run's code came to.
enum Attempted {
    /// The code returned: an action's attempt has the change to commit.
    Done {
        journal: Journal,
        value: Value,
        change: Option<Box<Change>>,
    },
    /// The code stopped at a step, or waited for a person's answer: what it did is rolled back,
    /// and the run goes on from its journal.
    Suspended { journal: Journal },
    /// The code trapped, ran out of fuel, or could not go on: what it did is rolled back.
    Failed {
        journal: Journal,
        error: RuntimeError,
    },
}

/// What an invocation the host sends on came to.
enum Delegated {
    /// It ran, or was refused or failed: what the run journals of it.
    Ran(Box<JournalEntry>),
    /// A person must answer it first.
    Asks,
}

/// The change an action's completed attempt makes.
struct Change {
    txn: Txn,
    queued: Vec<QueuedBlock>,
    proposed: Vec<(Principal, ActionRef)>,
    note: ChangeNote,
}

/// How a run whose code stopped came to be run again for its answer.
enum RunEnd {
    Done {
        journal: Journal,
        value: Value,
    },
    Suspended(Suggestion),
    Failed {
        journal: Journal,
        error: RuntimeError,
    },
}

/// A scalar result as the guest receives it: boxed like `some_i`, so it unwraps with
/// `unwrap_i` / `unwrap_f`. Handles and errors travel as they are.
fn boxed(v: Value) -> Value {
    match v {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) => Value::some(v),
        other => other,
    }
}

/// Whether an error is the action's own failure, rolled back with it, rather than a mistake in
/// the request or a store that could not hold it: what a caller sees as a failed invocation.
fn failed(e: &RuntimeError) -> bool {
    !matches!(
        e,
        RuntimeError::BadArgs(_)
            | RuntimeError::Store(_)
            | RuntimeError::NotFound(_)
            | RuntimeError::NotPending(_)
    )
}

fn push_chain(chain: &mut Vec<ModuleHash>, author: Option<&ModuleHash>) {
    if let Some(a) = author
        && chain.last() != Some(a)
    {
        chain.push(a.clone());
    }
}

/// The action id of `many(actions)`: one action applied to many items as one change. Its only
/// argument is the list of the action references, one per item, all naming the same action.
pub const MANY_ACTION: &str = "$many";

impl Core {
    // ----- marshalling -------------------------------------------------------------------

    /// Whether the reference `r` names a resource of the type `key`: one this runtime holds,
    /// or another app's, named by its app and type.
    fn ref_fits(&self, key: &crate::meta::TypeKey, r: &crate::value::RefValue) -> bool {
        let registry = self.registry.borrow();
        let Some(want) = registry.type_for(key) else {
            return false;
        };
        want == r.target.ty || crate::foreign::foreign_type(&r.target.ty).as_ref() == Some(key)
    }

    fn check_param(&self, ty: &Ty, v: &Value, into_action: bool) -> Result<(), String> {
        let ok = match (ty, v) {
            // A program's `host.Resource` is any resource.
            (Ty::Record(key), Value::Ref(_)) if *key == tilolang::resource_key() => true,
            (Ty::Record(key), Value::Ref(r)) => self.ref_fits(key, r),
            // A record built in a view (`DockItem(...)`) is a value of its type where it is read:
            // a view's or a function's parameter, as the shell's dock hands its running items to
            // its tiles. It names no resource, so it never reaches an action: what an action
            // changes in its record parameter must land in a resource, not in a copy that is
            // dropped (`marshal` refuses it there).
            (Ty::Record(key), Value::Rec(rec)) if !into_action => {
                self.registry.borrow().type_for(key).as_deref() == Some(rec.ty.as_str())
            }
            (Ty::Union(keys), Value::Ref(r)) => keys.iter().any(|key| self.ref_fits(key, r)),
            // A host value (an action, a lens, a type's metadata) is held as its fields, and so
            // is a filter, held as the `host.Filter` its catalogue entry is.
            (Ty::Record(key), Value::Tuple(_)) => tilolang::host::is_value(key),
            (Ty::Text | Ty::Date | Ty::Time | Ty::Identifier, Value::Text(_))
            | (Ty::RichText, Value::Rich(_))
            | (Ty::List(_) | Ty::Set(_) | Ty::Map(_) | Ty::Collection(_), Value::List(_))
            | (Ty::List(_) | Ty::Collection(_), Value::Coll(_))
            | (Ty::Option(_), Value::Opt(_))
            | (Ty::Enum(_) | Ty::Result(..), Value::Enum(_))
            | (Ty::Tuple(_) | Ty::Filter(_) | Ty::Sort(_), Value::Tuple(_))
            | (Ty::Action, Value::Action(_))
            | (Ty::Principal, Value::Principal(_))
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

    /// Turn arguments into wasm values in the current frame. An action's export (`a_<id>`, the
    /// ABI's name for an action) takes resources for its record parameters, never a record a
    /// view built.
    fn marshal(
        &self,
        pkg: &Package,
        export: &str,
        params: &[ParamMeta],
        args: &[Value],
    ) -> Result<Vec<Wasm>, String> {
        let into_action = export.starts_with("a_");
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
                let ty = parse_ty(&p.ty, &pkg.meta.app)?;
                let mismatch = || {
                    format!(
                        "argument `{}`: a {} is not a {}",
                        p.name,
                        v.kind_name(),
                        p.ty
                    )
                };
                match repr(&ty) {
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
                        self.check_param(&ty, v, into_action)
                            .map_err(|e| format!("argument `{}`: {e}", p.name))?;
                        self.frames.borrow_mut().alloc(v.clone()).map(Wasm::I32)
                    }
                }
            })
            .collect()
    }

    fn unmarshal(&self, app: &str, result: Option<&str>, w: Option<Wasm>) -> Result<Value, String> {
        let Some(ty) = result else {
            return Ok(Value::Unit);
        };
        let ty = parse_ty(ty, app)?;
        if ty == Ty::Unit {
            return Ok(Value::Unit);
        }
        match (repr(&ty), w) {
            (Repr::I64, Some(Wasm::I64(i))) => Ok(Value::Int(i)),
            (Repr::F64, Some(Wasm::F64(f))) => Ok(Value::Float(f)),
            (Repr::Bool, Some(Wasm::I32(i))) => Ok(Value::Bool(i != 0)),
            (Repr::Handle, Some(Wasm::I32(h))) => Ok(self.frames.borrow().get(h)?.clone()),
            _ => Err("the export returned a value of the wrong type".to_owned()),
        }
    }

    /// Marshal, call and unmarshal in the frame that is already pushed.
    pub(crate) fn call_in_frame(
        &self,
        pkg: &Package,
        export: &str,
        params: &[ParamMeta],
        args: &[Value],
        result: Option<&str>,
    ) -> Result<Value, RuntimeError> {
        let wasm_args = self
            .marshal(pkg, export, params, args)
            .map_err(RuntimeError::BadArgs)?;
        let host: std::rc::Rc<dyn Host> = self.rc().map_err(RuntimeError::Package)?;
        // What the call costs is charged to the unit that makes it, from here to its end: a
        // function called while it runs is a unit of its own, and the call it costs counts in
        // this one too.
        let measured = self.costs.on();
        if measured {
            let unit = pkg.unit(export).unwrap_or_else(|| {
                std::rc::Rc::new(crate::cost::Unit::unnamed(export, &pkg.meta.app))
            });
            self.costs.enter(unit, self.store.borrow().reads());
        }
        let out = self.engine.call(&pkg.hash, export, &wasm_args, host);
        if measured {
            self.costs.leave(self.store.borrow().reads());
        }
        self.unmarshal(&pkg.meta.app, result, out?)
            .map_err(RuntimeError::Package)
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

    /// See [`crate::Runtime::complete_args`].
    ///
    /// A [`Value::Missing`] is an argument the call left out: nothing stands for it, so the
    /// parameter its place names takes its default, as one left out at the end does.
    pub(crate) fn complete_args(
        &self,
        action: &str,
        given: Vec<Option<Value>>,
    ) -> Result<Vec<Value>, RuntimeError> {
        let given: Vec<Option<Value>> = given
            .into_iter()
            .map(|arg| arg.filter(|arg| *arg != Value::Missing))
            .collect();
        let Some(pkg) = self.registry.borrow().action_package(action) else {
            // A platform action (`$set`, `then`) no package declares takes what it is given.
            return given
                .into_iter()
                .map(|v| v.ok_or_else(|| RuntimeError::BadArgs("an argument is missing".into())))
                .collect();
        };
        let meta = pkg
            .action(action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {action}")))?
            .clone();
        if given.len() > meta.params.len() {
            return Err(RuntimeError::BadArgs(format!(
                "expected {} arguments, got {}",
                meta.params.len(),
                given.len()
            )));
        }
        let mut args: Vec<Value> = Vec::with_capacity(meta.params.len());
        let mut given = given.into_iter();
        for (i, param) in meta.params.iter().enumerate() {
            if let Some(v) = given.next().flatten() {
                args.push(v);
                continue;
            }
            let Some(default) = &param.default else {
                return Err(RuntimeError::BadArgs(format!(
                    "argument `{}` is missing, and it has no default",
                    param.name
                )));
            };
            // `.none` is a constant: an app that publishes its actions without a module (a
            // native app's, the session's) has no code to compute it.
            if default.is_none() {
                args.push(Value::Opt(None));
                continue;
            }
            let before = meta.params.get(..i).unwrap_or_default();
            let value = self.call_pure(&pkg, &default.export, before, &args, &param.ty)?;
            args.push(value);
        }
        Ok(args)
    }

    /// The parameters `aref` leaves out that have no default, by index: what an invocation of
    /// it still needs. Empty for anything but an action of a package.
    pub(crate) fn missing_arguments(&self, aref: &ActionRef) -> Vec<usize> {
        let Some(pkg) = self.registry.borrow().action_package(&aref.action) else {
            return Vec::new();
        };
        let Some(meta) = pkg.action(&aref.action) else {
            return Vec::new();
        };
        // The arguments up to the first the call left out are the ones it gave.
        let bound = aref
            .args
            .iter()
            .position(|arg| *arg == Value::Missing)
            .unwrap_or(aref.args.len());
        meta.unbound(bound).map(|(at, _)| at).collect()
    }

    /// The default of the parameter `at` of `action`, over the arguments before it: `None`
    /// when it has none.
    pub(crate) fn default_argument(
        &self,
        action: &str,
        at: usize,
        before: &[Value],
    ) -> Result<Option<Value>, RuntimeError> {
        let Some(pkg) = self.registry.borrow().action_package(action) else {
            return Ok(None);
        };
        let meta = pkg
            .action(action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {action}")))?
            .clone();
        let Some(param) = meta.params.get(at) else {
            return Ok(None);
        };
        let Some(default) = &param.default else {
            return Ok(None);
        };
        if default.is_none() {
            return Ok(Some(Value::Opt(None)));
        }
        let params = meta.params.get(..at).unwrap_or_default();
        if params.len() != before.len() {
            return Ok(None);
        }
        self.call_pure(&pkg, &default.export, params, before, &param.ty)
            .map(Some)
    }

    /// Call the export `export` of `pkg`, a function of the values it is given that changes
    /// nothing (a parameter default, an argument computed at an event), in a frame of its own.
    pub(crate) fn call_pure(
        &self,
        pkg: &Package,
        export: &str,
        params: &[ParamMeta],
        args: &[Value],
        result: &str,
    ) -> Result<Value, RuntimeError> {
        self.call_framed(pkg, FrameKind::Pure, export, params, args, Some(result))
    }

    /// Call the export `export` of `pkg` in a frame of `kind`, so that what it makes is authored
    /// by `pkg` and what it reads is what the code that called it sees.
    fn call_framed(
        &self,
        pkg: &Package,
        kind: FrameKind,
        export: &str,
        params: &[ParamMeta],
        args: &[Value],
        result: Option<&str>,
    ) -> Result<Value, RuntimeError> {
        self.frames.borrow_mut().push(pkg.hash.clone(), kind);
        let value = self.call_in_frame(pkg, export, params, args, result);
        self.frames.borrow_mut().pop();
        value
    }

    /// `call_fn`: another package's `fn`, as in `todo.finished(list, t)`. It runs as that
    /// package's own code, on the values it is given, in a frame authored by that package — so
    /// what it makes is its app's, and what it reads is what the code that called it sees. It
    /// changes nothing, so no authority decides it, as reading that package's resources is not
    /// decided either. A scalar result comes back boxed, as an action's does, for the caller to
    /// unbox.
    pub(crate) fn fn_call(&self, id_h: i32, args_h: i32) -> HResult<Option<Wasm>> {
        let id = self.text(id_h)?;
        let Value::List(args) = self.val(args_h)? else {
            return trap("expected the arguments of a call as a list");
        };
        let Some(pkg) = self.registry.borrow().fn_package(&id) else {
            return trap(format!("no app here declares the function `{id}`"));
        };
        if !pkg.code {
            return trap(format!(
                "the function `{id}` belongs to {}, which this device does not run",
                pkg.meta.app
            ));
        }
        let Some(func) = pkg.fn_(&id) else {
            return trap(format!("no app here declares the function `{id}`"));
        };
        if self.frames.borrow().calls() >= MAX_CALL_DEPTH {
            return trap(format!(
                "calls into other apps' functions are nested more than {MAX_CALL_DEPTH} deep"
            ));
        }
        let value = match self.call_framed(
            &pkg,
            FrameKind::Call,
            &format!("n_{id}"),
            &func.params,
            &args,
            Some(&func.result),
        ) {
            Ok(value) => value,
            Err(e) => return trap(e.to_string()),
        };
        let value = match value {
            v @ (Value::Int(_) | Value::Float(_) | Value::Bool(_)) => Value::some(v),
            v => v,
        };
        self.ret_h(value)
    }

    /// `aref` with the arguments it leaves out given their defaults, now.
    pub(crate) fn completed(&self, aref: &ActionRef) -> Result<ActionRef, RuntimeError> {
        let wanted = self
            .registry
            .borrow()
            .action_package(&aref.action)
            .and_then(|pkg| pkg.action(&aref.action).map(|a| a.params.len()));
        // The arguments up to the first the call left out are the ones it gave.
        let bound = aref
            .args
            .iter()
            .position(|arg| *arg == Value::Missing)
            .unwrap_or(aref.args.len());
        match wanted {
            Some(n) if n > bound => {
                let given = aref.args.iter().cloned().map(Some).collect();
                Ok(ActionRef {
                    args: self.complete_args(&aref.action, given)?,
                    ..aref.clone()
                })
            }
            _ => Ok(aref.clone()),
        }
    }

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
            suggested_by: ctx.suggested_by.as_ref().map(|p| p.0.clone()),
            from: ctx.from.clone(),
            at: ctx.at.clone(),
            ..ChangeNote::default()
        }
    }

    /// Run an action, and every action it invokes, as a run: one change per action it makes a
    /// change of its own, and one for it.
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
        let aref = &self.completed(aref)?;
        match self.start_ctx(ctx, aref) {
            Ok(RunOutcome::Done(v)) => Ok(ActionOutcome::Done(v)),
            Ok(RunOutcome::Suspended(s)) => Ok(ActionOutcome::Suspended(s)),
            Err(e) if failed(&e) => Ok(ActionOutcome::Failed(e.to_string())),
            Err(e) => Err(e),
        }
    }

    /// One action applied to many items (`many`), as one change.
    ///
    /// Each reference names the same action for a different item. They are decided once — one
    /// action, by one person, from one piece of code — and run in one transaction, so they are
    /// one change: one undo takes them all back, and history says how many items it changed.
    /// Each is a call at its own app's door when the action is another app's, which keeps its
    /// own record's changes apart.
    fn invoke_many(
        &self,
        invoker: &Principal,
        aref: &ActionRef,
        from: Option<&ResourceRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::List(items)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs(
                "`many` takes a list of actions".into(),
            ));
        };
        let mut refs = Vec::with_capacity(items.len());
        for item in items {
            let Value::Action(inner) = item else {
                return Err(RuntimeError::BadArgs(format!(
                    "`many` takes actions, not {}",
                    item.kind_name()
                )));
            };
            if inner.action == MANY_ACTION {
                return Err(RuntimeError::BadArgs(
                    "`many` takes the actions to apply, not another `many`".into(),
                ));
            }
            refs.push(self.completed(inner)?);
        }
        let Some(first) = refs.first() else {
            return Ok(InvokeOutcome::Done(Value::Unit));
        };
        if let Some(other) = refs.iter().find(|r| r.action != first.action) {
            return Err(RuntimeError::BadArgs(format!(
                "`many` applies one action over many items: it holds `{}` and `{}`",
                first.action, other.action
            )));
        }
        if self.foreign_action(&first.action) {
            let mut done = Value::Unit;
            for inner in &refs {
                match self.invoke_foreign(inner)? {
                    InvokeOutcome::Done(v) => done = v,
                    other => return Ok(other),
                }
            }
            return Ok(InvokeOutcome::Done(done));
        }
        let mut chain = Vec::new();
        push_chain(&mut chain, first.author.as_ref());
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            chain,
            approved_by: None,
            identity: None,
            from: from.cloned(),
            suggested_by: None,
            at: None,
        };
        match self.decide(&ctx, first)? {
            Decision::Allow => {
                self.run_many(&ctx, &refs)?;
                Ok(InvokeOutcome::Done(Value::Unit))
            }
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
                    action: None,
                    args: aref.args.clone(),
                    invoker: ctx.invoker,
                    chain: ctx.chain,
                    from: ctx.from,
                    at: ctx.at,
                    suggested_by: ctx.suggested_by.map(|who| who.0),
                    entries: Vec::new(),
                    pending: Some(Pending::Ask(suggestion.clone())),
                    finished: false,
                    approved_by: None,
                    effects: 0,
                    parent: None,
                    invocation: run.clone(),
                };
                self.store.borrow_mut().save_journal(&run, &journal)?;
                Ok(InvokeOutcome::Suggested(suggestion))
            }
        }
    }

    /// Run `refs`, each naming the same action for a different item, in one transaction and
    /// one change.
    ///
    /// # Errors
    ///
    /// As [`Core::run_action`].
    /// The run of one action applied to many items: one attempt, and no journal, because an
    /// action that calls another action has a change and a journal per call.
    fn many_run(&self, ctx: &ExecCtx, first: &ActionRef) -> RunState {
        let run = self.run_id(ctx);
        RunState {
            journal: Journal {
                action: Some(first.action.clone()),
                args: first.args.clone(),
                invoker: ctx.invoker.clone(),
                chain: ctx.chain.clone(),
                from: ctx.from.clone(),
                at: ctx.at.clone(),
                suggested_by: ctx.suggested_by.as_ref().map(|p| p.0.clone()),
                entries: Vec::new(),
                pending: None,
                finished: false,
                approved_by: ctx.approved_by.clone(),
                effects: 0,
                parent: None,
                invocation: run.clone(),
            },
            run,
            cursor: 0,
            invoker: ctx.invoker.clone(),
            suggested_by: ctx.suggested_by.clone(),
            clock: None,
            journaled: false,
        }
    }

    fn run_many(&self, ctx: &ExecCtx, refs: &[ActionRef]) -> Result<(), RuntimeError> {
        let Some(first) = refs.first() else {
            return Ok(());
        };
        let pkg = self
            .registry
            .borrow()
            .action_package(&first.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", first.action)))?;
        let meta = pkg
            .action(&first.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", first.action)))?
            .clone();
        // `many` is one change over many items: an action that calls another makes a change per
        // call, so it is no action `many` applies.
        if meta.invokes {
            return Err(RuntimeError::Trap(
                "`many` applies one action to many items as one change, and an action that \
                 calls another action makes a change per call"
                    .into(),
            ));
        }
        let txn = self.store.borrow_mut().begin()?;
        let primary = first.args.iter().find_map(|v| match v {
            Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
            _ => None,
        });
        let mut note = Self::note(ctx, &pkg, first);
        note.count = u32::try_from(refs.len()).unwrap_or(u32::MAX);
        self.frames.borrow_mut().push(
            pkg.hash.clone(),
            FrameKind::Action(ActionState {
                txn,
                action: first.action.clone(),
                primary,
                invoker: ctx.invoker.clone(),
                suggested_by: ctx.suggested_by.clone(),
                suggestions: Vec::new(),
                queued: Vec::new(),
                scenario: meta.scenario,
                invocations: Vec::new(),
                note: Box::new(note),
                touched: std::collections::BTreeSet::new(),
                run: Box::new(self.many_run(ctx, first)),
            }),
        );
        let mut failure = None;
        for aref in refs {
            if let Err(e) = self.call_in_frame(
                &pkg,
                &format!("a_{}", aref.action),
                &meta.params,
                &aref.args,
                meta.result.as_deref(),
            ) {
                failure = Some(e);
                break;
            }
        }
        let at = self.change_time();
        let frame = self.frames.borrow_mut().pop();
        let (queued, proposed, txn, note) = match frame.map(|f| f.kind) {
            Some(FrameKind::Action(a)) => {
                let mut note = *a.note;
                note.at = Some(at);
                note.touched = a.touched.into_iter().collect();
                // The one change records what the run over every item read, in order.
                note.effects = a
                    .run
                    .journal
                    .entries
                    .iter()
                    .filter(|entry| entry.is_outside_effect())
                    .cloned()
                    .collect();
                (a.queued, a.suggestions, a.txn, note)
            }
            _ => (Vec::new(), Vec::new(), txn, Self::note(ctx, &pkg, first)),
        };
        match failure {
            None => {
                self.store.borrow_mut().commit(txn, &note)?;
                self.settle_blocks(&ctx.invoker, queued)?;
                for (who, suggested) in proposed {
                    self.propose(&who, &suggested)?;
                }
                Ok(())
            }
            Some(RuntimeError::BadArgs(m)) => {
                self.store.borrow_mut().rollback(txn);
                Err(RuntimeError::BadArgs(m))
            }
            Some(e) => {
                self.store.borrow_mut().rollback(txn);
                Err(RuntimeError::Trap(e.to_string()))
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
        self.invoke_direct_from(invoker, aref, aref.from.as_ref())
    }

    /// [`Core::invoke_direct`] of an invocation made from the resource `from`: what the
    /// resources it creates were made from, recorded in the change it makes.
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub fn invoke_direct_from(
        &self,
        invoker: &Principal,
        aref: &ActionRef,
        from: Option<&ResourceRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_as(None, invoker, aref, from)
    }

    /// [`Core::invoke_direct_from`] as the run `run`: an invocation whose run every replica can
    /// name the same way (a trigger, run for a record at an instant, on whichever device comes
    /// to run it) runs as that run on each of them, so what it queues and writes is held once.
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub fn invoke_direct_named(
        &self,
        run: &RunId,
        invoker: &Principal,
        aref: &ActionRef,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_as(Some(run), invoker, aref, aref.from.as_ref())
    }

    fn invoke_direct_as(
        &self,
        identity: Option<&RunId>,
        invoker: &Principal,
        aref: &ActionRef,
        from: Option<&ResourceRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        // One action over many items is one change: its references are the change's own.
        if aref.action == MANY_ACTION {
            return self.invoke_many(invoker, aref, from);
        }
        // What the invoker left out is computed now, on what they see, and recorded.
        let aref = &self.completed(aref)?;
        // An action of an app this runtime does not run is a child invocation at that app's
        // own door: the host sends it on with this view's author, and the door decides it
        // there (action-system.md, "One invocation path").
        if self.foreign_action(&aref.action) {
            return self.invoke_foreign(aref);
        }
        let mut chain = Vec::new();
        push_chain(&mut chain, aref.author.as_ref());
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            chain,
            approved_by: None,
            suggested_by: None,
            identity: identity.cloned(),
            from: from.cloned(),
            at: None,
        };
        match self.decide(&ctx, aref)? {
            Decision::Allow => match self.run_action(&ctx, aref)? {
                ActionOutcome::Done(v) => Ok(InvokeOutcome::Done(v)),
                ActionOutcome::Suspended(suggestion) => Ok(InvokeOutcome::Suggested(suggestion)),
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
                    action: None,
                    args: aref.args.clone(),
                    invoker: ctx.invoker,
                    chain: ctx.chain,
                    from: ctx.from,
                    at: ctx.at,
                    suggested_by: ctx.suggested_by.map(|who| who.0),
                    entries: Vec::new(),
                    pending: Some(Pending::Ask(suggestion.clone())),
                    finished: false,
                    approved_by: None,
                    effects: 0,
                    parent: None,
                    invocation: run.clone(),
                };
                self.store.borrow_mut().save_journal(&run, &journal)?;
                Ok(InvokeOutcome::Suggested(suggestion))
            }
        }
    }

    /// Whether `action` is an action of an app this runtime does not run: one the host invokes
    /// at that app's door.
    fn foreign_action(&self, action: &str) -> bool {
        self.registry
            .borrow()
            .action_package(action)
            .is_some_and(|pkg| pkg.foreign)
    }

    /// Invoke `aref` where its app runs: the host's invoker sends it on as a child of the code
    /// that named it, and the answer is the child's.
    fn invoke_foreign(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        use crate::delegate::Invoked;
        let invocation = self.invocation_of(aref)?;
        let answer = match self.invoker.borrow().as_ref() {
            Some(invoker) => invoker.invoke(&invocation),
            None => {
                return Err(RuntimeError::Package(format!(
                    "`{}` is an action of `{}`, an app this host does not run, and it makes no \
                     invocation of it",
                    aref.action,
                    invocation.app()
                )));
            }
        };
        Ok(match answer {
            Invoked::Done(result) => InvokeOutcome::Done(result),
            Invoked::Failed(reason) => return Err(RuntimeError::Trap(reason)),
            // The door said no, or asks the person first: nothing ran here. A host that asked
            // keeps what it said, and answers the person's decision itself.
            Invoked::Refused(_) | Invoked::Asks(_) => InvokeOutcome::Denied,
        })
    }

    /// Start a run of `action` as `invoker`, with the authorship `chain` that led to it: what a
    /// program runs as.
    ///
    /// # Errors
    ///
    /// An unknown action, arguments that do not fit, a trap, or a store failure.
    pub fn start_run(
        &self,
        invoker: &Principal,
        action: &str,
        args: Vec<Value>,
        chain: Vec<ModuleHash>,
    ) -> Result<RunOutcome, RuntimeError> {
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            chain,
            approved_by: None,
            suggested_by: None,
            identity: None,
            from: None,
            at: None,
        };
        self.start_ctx(
            &ctx,
            &self.completed(&ActionRef::new(action.to_owned(), args))?,
        )
    }

    /// The id a run started for `ctx` has: the one the invocation it stands for already has,
    /// else one of its own. Every effect it queues is keyed by it, so a device that runs the
    /// same invocation queues each of them under the key the device that ran it first did.
    fn run_id(&self, ctx: &ExecCtx) -> RunId {
        ctx.identity
            .clone()
            .unwrap_or_else(|| self.store.borrow_mut().fresh_id("r"))
    }

    /// Start a run of `aref` for `ctx`: a journal, and the code run until it finishes or waits
    /// for a person. Nothing is written until it does.
    fn start_ctx(&self, ctx: &ExecCtx, aref: &ActionRef) -> Result<RunOutcome, RuntimeError> {
        let run = self.run_id(ctx);
        let journal = Journal {
            action: Some(aref.action.clone()),
            args: aref.args.clone(),
            invoker: ctx.invoker.clone(),
            chain: ctx.chain.clone(),
            from: ctx.from.clone(),
            at: ctx.at.clone(),
            suggested_by: ctx.suggested_by.as_ref().map(|p| p.0.clone()),
            entries: Vec::new(),
            pending: None,
            finished: false,
            approved_by: ctx.approved_by.clone(),
            effects: 0,
            parent: None,
            invocation: run.clone(),
        };
        self.drive_chain(&run, journal)
    }

    /// Run `journal`'s action until it finishes or waits for a person, and answer the run that
    /// invoked it as a step, if one did.
    ///
    /// The run at the top of the calls is one action of the workspace: everything it writes,
    /// and everything the actions it calls write, is kept apart from what is published until
    /// it ends, and a run that fails anywhere hands all of it back to the store to take back
    /// out ([`Store::abort_run`]). A run that waits for a person ends there, and goes on as a
    /// new one.
    fn drive_chain(&self, run: &RunId, journal: Journal) -> Result<RunOutcome, RuntimeError> {
        let outermost = !self.running.replace(true);
        let invocation = journal.invocation.clone();
        if outermost && !invocation.is_empty() {
            self.store.borrow_mut().begin_run(&invocation);
        }
        let outcome = match self.drive(run, journal) {
            Ok(RunEnd::Done { journal, value }) => self.answer_parent(&journal, Ok(value)),
            Ok(RunEnd::Suspended(suggestion)) => Ok(RunOutcome::Suspended(suggestion)),
            Ok(RunEnd::Failed { journal, error }) => self.answer_parent(&journal, Err(error)),
            Err(error) => Err(error),
        };
        if outermost {
            self.running.set(false);
            if !invocation.is_empty() {
                let mut store = self.store.borrow_mut();
                match &outcome {
                    Ok(_) => store.end_run(&invocation),
                    Err(_) => store.abort_run(&invocation),
                }
            }
        }
        outcome
    }

    /// Answer the run `journal` was a step of, if it was: its outcome is that run's next
    /// journaled entry, and that run goes on. Written before the step's own journal says it
    /// finished, so a run interrupted between the two runs the step once.
    fn answer_parent(
        &self,
        journal: &Journal,
        outcome: Result<Value, RuntimeError>,
    ) -> Result<RunOutcome, RuntimeError> {
        let Some(parent) = journal.parent.clone() else {
            return match outcome {
                Ok(value) => Ok(RunOutcome::Done(value)),
                Err(e) => Err(e),
            };
        };
        let action = journal.action.clone().unwrap_or_default();
        let entry = match &outcome {
            Ok(value) => JournalEntry::Invoked {
                action,
                result: value.clone(),
            },
            Err(e) => JournalEntry::Failed {
                action,
                reason: e.to_string(),
            },
        };
        let mut parent_journal = self
            .store
            .borrow_mut()
            .load_journal(&parent)?
            .ok_or_else(|| RuntimeError::NotFound(format!("run {parent}")))?;
        parent_journal.entries.push(entry);
        parent_journal.pending = None;
        self.store
            .borrow_mut()
            .save_journal(&parent, &parent_journal)?;
        self.drive_chain(&parent, parent_journal)
    }

    /// Run the code of `run` from the start of its journal until it finishes, stops, or waits
    /// for a person.
    fn drive(&self, run: &RunId, journal: Journal) -> Result<RunEnd, RuntimeError> {
        let Some(action) = journal.action.clone() else {
            return Err(RuntimeError::NotFound("an action for this run".into()));
        };
        let (pkg, meta) = {
            let registry = self.registry.borrow();
            let pkg = registry
                .action_package(&action)
                .ok_or_else(|| RuntimeError::NotFound(format!("action {action}")))?;
            let meta = pkg
                .action(&action)
                .ok_or_else(|| RuntimeError::NotFound(format!("action {action}")))?
                .clone();
            (pkg, meta)
        };
        // An agent's program is code its agent wrote: what it applies is decided as the agent's
        // code, as a block's is.
        let program = pkg.meta.app.starts_with(tilolang::PROGRAM_APP_PREFIX);
        let module = if program {
            ModuleHash::principal(&journal.invoker.0)
        } else {
            pkg.hash.clone()
        };
        // Only code that calls another action is run again, so only it journals its reads and
        // its writes. Anything else commits one change and is never replayed.
        let journaled = program || meta.invokes;
        // A run a scenario made act at a moment reads its clock as that moment, in the person's
        // zone: `now`, `today` and the time its change is recorded at all follow it.
        let clock = journal.at.clone().map(|now| crate::run::Clock {
            now,
            zone: self.zone_name.borrow().as_ref().and_then(|name| name()),
            offset: self.zone.borrow().as_ref().map_or(0, |zone| zone()),
        });
        let mut journal = journal;
        loop {
            let state = RunState {
                run: run.clone(),
                cursor: 0,
                invoker: journal.invoker.clone(),
                suggested_by: journal.suggested_by.clone().map(Principal),
                clock: clock.clone(),
                journaled,
                journal,
            };
            match self.attempt(&pkg, &meta, &module, program, state)? {
                Attempted::Done {
                    journal: next,
                    value,
                    change,
                } => {
                    journal = next;
                    if let Some(change) = change {
                        self.commit_segment(&journal.invoker, *change)?;
                    }
                    journal.finished = true;
                    journal.pending = None;
                    return Ok(RunEnd::Done { journal, value });
                }
                Attempted::Suspended { journal: next } => {
                    journal = next;
                    match journal.pending.clone() {
                        Some(Pending::Ask(suggestion)) => {
                            self.store.borrow_mut().save_journal(run, &journal)?;
                            return Ok(RunEnd::Suspended(suggestion));
                        }
                        Some(Pending::Step(step)) => {
                            let waited = self.take_step(run, &mut journal, &step)?;
                            if let Some(suggestion) = waited {
                                // The step asks: the run now waits for the person, and a
                                // resume goes on from here.
                                self.store.borrow_mut().save_journal(run, &journal)?;
                                return Ok(RunEnd::Suspended(suggestion));
                            }
                        }
                        None => {
                            return Err(RuntimeError::Package(
                                "the run suspended with nothing it waits for".into(),
                            ));
                        }
                    }
                }
                Attempted::Failed {
                    journal: next,
                    error,
                } => {
                    return Ok(RunEnd::Failed {
                        journal: next,
                        error,
                    });
                }
            }
        }
    }

    /// The context a run's code runs in, as its journal recorded it when it started.
    fn ctx_of(run: &RunId, journal: &Journal) -> ExecCtx {
        ExecCtx {
            invoker: journal.invoker.clone(),
            chain: journal.chain.clone(),
            approved_by: journal.approved_by.clone(),
            suggested_by: journal.suggested_by.clone().map(Principal),
            identity: Some(run.clone()),
            from: journal.from.clone(),
            at: journal.at.clone(),
        }
    }

    /// One attempt at the code of `run`: the transaction it writes in, and the frame it runs in.
    fn attempt(
        &self,
        pkg: &Package,
        meta: &ActionMeta,
        module: &ModuleHash,
        program: bool,
        state: RunState,
    ) -> Result<Attempted, RuntimeError> {
        let txn = if program {
            None
        } else {
            Some(self.store.borrow_mut().begin()?)
        };
        let action = state.journal.action.clone().unwrap_or_default();
        let args = state.journal.args.clone();
        let ctx = Self::ctx_of(&state.run, &state.journal);
        let primary = args.iter().find_map(|v| match v {
            Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
            _ => None,
        });
        let mut note = Self::note(&ctx, pkg, &ActionRef::new(action.clone(), args.clone()));
        // A change of a run, and of every action it called, names the one invocation: what
        // makes the run one action of the workspace.
        if !state.journal.invocation.is_empty() {
            note.invocation = Some(state.journal.invocation.clone());
        }
        let kind = match txn {
            Some(txn) => FrameKind::Action(ActionState {
                txn,
                action: action.clone(),
                primary,
                invoker: ctx.invoker.clone(),
                suggested_by: ctx.suggested_by,
                suggestions: Vec::new(),
                queued: Vec::new(),
                scenario: meta.scenario,
                invocations: Vec::new(),
                note: Box::new(note),
                touched: std::collections::BTreeSet::new(),
                run: Box::new(state),
            }),
            None => FrameKind::Program(Box::new(state)),
        };
        self.frames.borrow_mut().push(module.clone(), kind);
        let result = self.call_in_frame(
            pkg,
            &format!("a_{action}"),
            &meta.params,
            &args,
            meta.result.as_deref(),
        );
        let at = self.change_time();
        let Some(frame) = self.frames.borrow_mut().pop() else {
            return Err(RuntimeError::Package("the run's frame vanished".into()));
        };
        let (journal, change) = match frame.kind {
            FrameKind::Program(state) => (state.journal, None),
            FrameKind::Action(a) => {
                let mut note = *a.note;
                note.at = Some(at);
                note.touched = a.touched.into_iter().collect();
                // What the run read of the world outside Tilo on its way, beside the change it
                // makes: the moment and the zone it read, and what it fetched or drew
                // (convergence.md, "Actions").
                note.effects = a
                    .run
                    .journal
                    .entries
                    .iter()
                    .filter(|entry| entry.is_outside_effect())
                    .cloned()
                    .collect();
                let change = Change {
                    txn: a.txn,
                    queued: a.queued,
                    proposed: a.suggestions,
                    note,
                };
                (a.run.journal, Some(Box::new(change)))
            }
            _ => return Err(RuntimeError::Package("the run's frame vanished".into())),
        };
        match result {
            Ok(value) => Ok(Attempted::Done {
                journal,
                value,
                change,
            }),
            Err(error) => {
                if matches!(error, RuntimeError::Engine(CallError::Suspended)) {
                    // It stopped to make a call: what it wrote until then is a change of its
                    // own, kept while the call runs. Its journal answers those writes when it
                    // goes on, so they are made once, and the call's change stands on them.
                    if let Some(change) = change {
                        self.commit_segment(&journal.invoker, *change)?;
                    }
                    Ok(Attempted::Suspended { journal })
                } else {
                    if let Some(change) = &change {
                        self.store.borrow_mut().rollback(change.txn);
                    }
                    Ok(Attempted::Failed { journal, error })
                }
            }
        }
    }

    /// Commit `change` as a change of `invoker`'s run, and do what it left for others: settle the
    /// blocks it settled and record what it proposed.
    fn commit_segment(&self, invoker: &Principal, change: Change) -> Result<(), RuntimeError> {
        self.store.borrow_mut().commit(change.txn, &change.note)?;
        self.settle_blocks(invoker, change.queued)?;
        for (who, suggested) in change.proposed {
            self.propose(&who, &suggested)?;
        }
        Ok(())
    }

    /// Run the action `step` names as a change of its own, as a step of `run`, and journal what
    /// it came to. `Ok(None)` when it ran and the run can go on; `Ok(Some(suggestion))` when it
    /// waits on a person first.
    fn take_step(
        &self,
        run: &RunId,
        journal: &mut Journal,
        step: &Step,
    ) -> Result<Option<Suggestion>, RuntimeError> {
        let aref = self.completed(&ActionRef {
            action: step.action.clone(),
            args: step.args.clone(),
            author: step.author.clone(),
            from: None,
        })?;
        let mut chain = journal.chain.clone();
        push_chain(&mut chain, aref.author.as_ref());
        let ctx = ExecCtx {
            // A scenario's `invoke(..., by:)` names who runs the call: the callee is decided and
            // recorded as that person, not as the run's own invoker.
            invoker: step.who.clone().unwrap_or_else(|| journal.invoker.clone()),
            chain,
            approved_by: None,
            suggested_by: journal.suggested_by.clone().map(Principal),
            identity: None,
            from: step.from.clone().or_else(|| journal.from.clone()),
            at: step.at.clone(),
        };
        // A host that holds no records sends the invocation on to the app that does.
        if self.invoker.borrow().is_some() {
            let invocation = self.invocation_of(&aref)?;
            match self.delegated_step(&aref.action, &invocation)? {
                Delegated::Ran(entry) => {
                    journal.entries.push(*entry);
                    Ok(None)
                }
                Delegated::Asks => Ok(Some(Self::ask(run, journal, &ctx, &aref))),
            }
        } else {
            let d = if step.seed {
                Decision::Allow
            } else {
                self.decide(&ctx, &aref)?
            };
            match d {
                Decision::Allow => self.run_step(run, journal, &ctx, &aref),
                Decision::Deny => {
                    journal.entries.push(JournalEntry::Denied {
                        action: step.action.clone(),
                        reason: "denied by authority".to_owned(),
                    });
                    Ok(None)
                }
                Decision::Ask => Ok(Some(Self::ask(run, journal, &ctx, &aref))),
            }
        }
    }

    /// Record that `run` waits for a person's answer to `aref`, and hand the suggestion out.
    fn ask(run: &RunId, journal: &mut Journal, ctx: &ExecCtx, aref: &ActionRef) -> Suggestion {
        let suggestion = Suggestion {
            run: run.clone(),
            action: aref.action.clone(),
            args: aref.args.clone(),
            invoker: ctx.invoker.clone(),
            chain: ctx.chain.clone(),
        };
        journal.pending = Some(Pending::Ask(suggestion.clone()));
        suggestion
    }

    /// Run `step` as a run of its own, a change of its own, invoked by `run`: its outcome is
    /// `run`'s next journaled entry, unless it waits on a person, in which case `run` waits with
    /// it and its outcome comes back here when they answer.
    fn run_step(
        &self,
        run: &RunId,
        journal: &mut Journal,
        ctx: &ExecCtx,
        aref: &ActionRef,
    ) -> Result<Option<Suggestion>, RuntimeError> {
        // The call is a step of the run: the run's effects before it and its calls after it
        // are the same on a device that runs the invocation again from the top, so what the
        // call does is held under the keys the first device used.
        let child = format!("{run}/{}", journal.entries.len());
        let child_journal = Journal {
            action: Some(aref.action.clone()),
            args: aref.args.clone(),
            invoker: ctx.invoker.clone(),
            chain: ctx.chain.clone(),
            from: ctx.from.clone(),
            at: ctx.at.clone(),
            suggested_by: ctx.suggested_by.as_ref().map(|p| p.0.clone()),
            entries: Vec::new(),
            pending: None,
            finished: false,
            approved_by: ctx.approved_by.clone(),
            effects: 0,
            parent: Some(run.clone()),
            invocation: journal.invocation.clone(),
        };
        match self.drive(&child, child_journal)? {
            RunEnd::Done { value, .. } => {
                journal.entries.push(JournalEntry::Invoked {
                    action: aref.action.clone(),
                    result: value,
                });
                Ok(None)
            }
            RunEnd::Failed { error, .. } => {
                journal.entries.push(JournalEntry::Failed {
                    action: aref.action.clone(),
                    reason: error.to_string(),
                });
                Ok(None)
            }
            RunEnd::Suspended(suggestion) => {
                self.store.borrow_mut().save_journal(run, journal)?;
                Ok(Some(suggestion))
            }
        }
    }

    /// Answer a suggestion, and run on from it. Approving runs the suggested action with the
    /// approver recorded; declining journals a refusal. A run then runs again from its journal.
    ///
    /// # Errors
    ///
    /// `NotPending` when nothing waits on this run.
    pub fn resolve(
        &self,
        run: &RunId,
        approver: &Principal,
        resolution: Resolution,
    ) -> Result<RunOutcome, RuntimeError> {
        let mut journal = self
            .store
            .borrow_mut()
            .load_journal(run)?
            .ok_or_else(|| RuntimeError::NotFound(format!("run {run}")))?;
        let pending = match journal.pending.take() {
            Some(pending) if !journal.finished => pending,
            _ => return Err(RuntimeError::NotPending(run.clone())),
        };
        let Pending::Ask(suggestion) = pending else {
            // A step waits on no person: it runs as soon as the run gets to it.
            journal.pending = Some(pending);
            return Err(RuntimeError::NotPending(run.clone()));
        };
        // The wait split the run: what it wrote before the call was one action, and what it does
        // on from the answer is another, under an invocation of its own.
        journal.invocation = self.store.borrow_mut().fresh_id("i");
        // The run goes on as itself: the effects the answer queues are keyed by its id, which a
        // device that held no journal of it reads from the record and runs it under too.
        let ctx = ExecCtx {
            invoker: suggestion.invoker.clone(),
            chain: suggestion.chain.clone(),
            approved_by: Some(approver.clone()),
            suggested_by: journal.suggested_by.clone().map(Principal),
            identity: Some(run.clone()),
            from: journal.from.clone(),
            at: journal.at.clone(),
        };
        let aref = ActionRef::new(suggestion.action.clone(), suggestion.args.clone());
        // A run that is only a suggestion waits no further: what it approves is its whole
        // action, and the change it makes is its.
        if journal.action.is_none() {
            journal.finished = true;
            self.store.borrow_mut().save_journal(run, &journal)?;
            return match resolution {
                Resolution::Approve => self.start_ctx(&ctx, &aref),
                Resolution::Decline => Ok(RunOutcome::Done(Value::Error(format!(
                    "declined by {approver}"
                )))),
            };
        }
        match resolution {
            Resolution::Approve => {
                // Its chain already ends in the module that called it.
                let aref = ActionRef::new(suggestion.action, suggestion.args);
                if let Some(deeper) = self.run_step(run, &mut journal, &ctx, &aref)? {
                    return Ok(RunOutcome::Suspended(deeper));
                }
            }
            Resolution::Decline => journal.entries.push(JournalEntry::Denied {
                action: suggestion.action,
                reason: format!("declined by {approver}"),
            }),
        }
        self.drive_chain(run, journal)
    }

    /// Run a read of a resource. A run that may invoke another action records the value in its
    /// journal, and running it again answers from the journal; anywhere else the read just runs.
    pub(crate) fn journaled_read(&self, live: impl FnOnce() -> HResult<Value>) -> HResult<Value> {
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut().filter(|run| run.journaled) else {
                drop(frames);
                return live();
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Read { value } = entry else {
                    return trap("the run diverged from its journal: expected a read");
                };
                let value = value.clone();
                run.cursor = run.cursor.saturating_add(1);
                return Ok(value);
            }
        }
        let value = live()?;
        let mut frames = self.frames.borrow_mut();
        if let Some(run) = frames.run_mut().filter(|run| run.journaled) {
            run.push(JournalEntry::Read {
                value: value.clone(),
            });
        }
        Ok(value)
    }

    /// Apply a write the running action makes, as [`Core::journaled_read`] answers a read: a run
    /// that is run again after it waited made this write before the call it waited on, and the
    /// change it made is in the record. Replay answers the write from the journal and applies
    /// nothing, so the item it made keeps what the call wrote on top of it.
    pub(crate) fn journaled_apply(
        &self,
        txn: Txn,
        at: &ResourceRef,
        m: &Mutation,
    ) -> HResult<Applied> {
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut().filter(|run| run.journaled) else {
                drop(frames);
                return self.apply_write(txn, at, m);
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Wrote { item } = entry else {
                    return trap("the run diverged from its journal: expected a write");
                };
                let applied = item.clone().map_or(Applied::Done, Applied::Item);
                run.cursor = run.cursor.saturating_add(1);
                return Ok(applied);
            }
        }
        let applied = self.apply_write(txn, at, m)?;
        let item = match &applied {
            Applied::Item(id) => Some(id.clone()),
            Applied::Done => None,
        };
        self.journal_push(JournalEntry::Wrote { item })?;
        Ok(applied)
    }

    /// Write `m` to `at` in `txn`, as the store takes it.
    fn apply_write(&self, txn: Txn, at: &ResourceRef, m: &Mutation) -> HResult<Applied> {
        self.store
            .borrow_mut()
            .apply(txn, at, m)
            .map_err(|e| HostFail::Trap(e.to_string()))
    }

    /// Records an entry in the run's journal.
    pub(crate) fn journal_push(&self, entry: JournalEntry) -> HResult<()> {
        let mut frames = self.frames.borrow_mut();
        let Some(run) = frames.run_mut() else {
            return trap("not in a run");
        };
        run.push(entry);
        Ok(())
    }

    /// `invoke(aref)` and `try act(args)`: the action runs as a change of its own at its own
    /// door. The journal answers a call the run already made; otherwise the run stops here, and
    /// the call runs and the code goes on from its result.
    fn invoke(&self, h: i32) -> HResult<Option<Wasm>> {
        let Value::Action(aref) = self.val(h)? else {
            return trap("expected an action reference");
        };
        // What the call runs under: the invocation a scenario pushed for it, else the run's own.
        // The invocation names who runs it and the moment it acts at; the resource its changes
        // are made from is the invocation's, else the run's. A scenario's code is the host's own
        // making, so what it calls is seed data too: it runs without asking.
        let (from, who, at, seed) = {
            let frames = self.frames.borrow();
            match frames.top().map(|f| &f.kind) {
                Ok(FrameKind::Action(state)) => {
                    let invocation = state.invocations.last();
                    (
                        invocation
                            .and_then(|inv| inv.from.clone())
                            .or_else(|| state.note.from.clone()),
                        invocation.map(|inv| inv.who.clone()),
                        invocation.and_then(|inv| inv.at.clone()),
                        state.scenario,
                    )
                }
                _ => (aref.from.clone(), None, None, false),
            }
        };
        let replayed = {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("only an action or a program invokes an action");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let value = replay_value(entry, &aref.action)?;
                run.cursor = run.cursor.saturating_add(1);
                Some(value)
            } else {
                run.journal.pending = Some(Pending::Step(Step {
                    action: aref.action.clone(),
                    args: aref.args.clone(),
                    author: aref.author,
                    from,
                    who,
                    at,
                    seed,
                }));
                None
            }
        };
        let Some(value) = replayed else {
            return Err(HostFail::Suspend);
        };
        // What did not run stops a program that does not `try` it: it never goes on as if it
        // had.
        if let Value::Error(reason) = &value
            && !self.trying.get()
            && self.frames.borrow().program()
        {
            return trap(reason.clone());
        }
        self.ret_h(boxed(value))
    }

    /// The invocation a task's host sends on for `aref`: the declared action with its
    /// arguments by name.
    fn invocation_of(&self, aref: &ActionRef) -> Result<crate::delegate::Invocation, RuntimeError> {
        let pkg = self
            .registry
            .borrow()
            .action_package(&aref.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", aref.action)))?;
        let meta = pkg
            .action(&aref.action)
            .ok_or_else(|| RuntimeError::NotFound(format!("action {}", aref.action)))?;
        Ok(crate::delegate::Invocation::Action {
            app: pkg.meta.app.clone(),
            action: aref.action.clone(),
            name: meta.name.clone(),
            // A flag travels as the integer the guest boxed it as; the invocation carries it
            // as the flag its parameter declares, so the app it goes to reads a flag.
            args: meta
                .params
                .iter()
                .zip(aref.args.iter())
                .map(|(p, arg)| {
                    let arg = match (p.ty.as_str(), arg) {
                        ("Bool", Value::Int(flag)) => Value::Bool(*flag != 0),
                        _ => arg.clone(),
                    };
                    (p.name.clone(), arg)
                })
                .collect(),
            class: meta.class,
            destructive: meta.role.as_deref() == Some("destructive"),
            author: aref.author.clone(),
            // The resource an action is on is its first parameter, when that is of one of its
            // package's own types (`focus(window: Window)`); a resource it takes otherwise
            // (`place(resource: host.Resource)`) is an argument, not where it is sent.
            subject: meta
                .params
                .first()
                .filter(|first| match parse_ty(&first.ty, &pkg.meta.app) {
                    Ok(Ty::Record(key)) => key.app == pkg.meta.app,
                    Ok(Ty::Union(keys)) => keys.iter().all(|key| key.app == pkg.meta.app),
                    _ => false,
                })
                .and_then(|_| aref.args.first())
                .and_then(|arg| match arg {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                }),
            from: aref.from.clone(),
        })
    }

    /// Send `invocation` (journaled as `action`) to the host's invoker: a host that holds no
    /// records makes every change a program asks for there, and answers with what it came to.
    fn delegated_step(
        &self,
        action: &str,
        invocation: &crate::delegate::Invocation,
    ) -> Result<Delegated, RuntimeError> {
        use crate::delegate::Invoked;
        let answer = match self.invoker.borrow().as_ref() {
            Some(invoker) => invoker.invoke(invocation),
            None => {
                return Err(RuntimeError::Package(
                    "this host sends no invocation on".to_owned(),
                ));
            }
        };
        Ok(match answer {
            Invoked::Done(result) => Delegated::Ran(Box::new(JournalEntry::Invoked {
                action: action.to_owned(),
                result,
            })),
            Invoked::Failed(reason) => Delegated::Ran(Box::new(JournalEntry::Failed {
                action: action.to_owned(),
                reason,
            })),
            Invoked::Refused(reason) => Delegated::Ran(Box::new(JournalEntry::Denied {
                action: action.to_owned(),
                reason,
            })),
            Invoked::Asks(_) => Delegated::Asks,
        })
    }

    /// `r.f = v` in a program: the field edit of `r`, sent to the host's invoker as an
    /// invocation of the app that holds `r`. One the policy refuses or that fails stops the
    /// program with why; one the person is asked about first stops it there.
    pub(crate) fn edit_field(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        use crate::delegate::{Invocation, Invoked};
        let target = self.ref_arg(a.h(0)?)?;
        let field_id = self.text(a.h(1)?)?;
        let value = crate::host_res::unboxed(self.val(a.h(2)?)?);
        let (app, field) = {
            let registry = self.registry.borrow();
            let Some((pkg, info)) = registry.type_info(&target.target.ty) else {
                return trap(format!("no app declares the type {}", target.target.ty));
            };
            let Some(field) = info.fields.iter().find(|f| f.id == field_id) else {
                return trap(format!("{} has no field {field_id}", info.name));
            };
            (pkg.meta.app.clone(), field.clone())
        };
        // A flag travels as the integer the guest boxed it as.
        let value = match (&field.ty, value) {
            (crate::meta::Ty::Bool, Value::Int(flag)) => Value::Bool(flag != 0),
            (_, value) => value,
        };
        let field = field.name;
        let action = format!("edit:{field_id}");
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("a field is set by a program only");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let replayed = replay_value(entry, &action)?;
                run.cursor = run.cursor.saturating_add(1);
                return match replayed {
                    Value::Error(reason) => trap(reason),
                    _ => ret_void(),
                };
            }
        }
        let invocation = Invocation::Edit {
            app,
            target: ResourceRef::clone(&target.target),
            field,
            field_id,
            value,
        };
        let answer = match self.invoker.borrow().as_ref() {
            Some(invoker) => invoker.invoke(&invocation),
            None => {
                return trap(
                    "a program changes a field through its app: this host sends no change on",
                );
            }
        };
        let (entry, failed) = match answer {
            Invoked::Done(_) => (
                JournalEntry::Invoked {
                    action,
                    result: Value::Unit,
                },
                None,
            ),
            Invoked::Failed(reason) => (
                JournalEntry::Failed {
                    action,
                    reason: reason.clone(),
                },
                Some(reason),
            ),
            Invoked::Refused(reason) => (
                JournalEntry::Denied {
                    action,
                    reason: reason.clone(),
                },
                Some(reason),
            ),
            Invoked::Asks(_) => return Err(HostFail::Suspend),
        };
        self.journal_push(entry)?;
        match failed {
            Some(reason) => trap(reason),
            None => ret_void(),
        }
    }

    /// `try act(args)`: invokes like `invoke` and hands the guest a `Result`: `.ok` with the
    /// action's result (none for a unit action), or `.err` with the reason it did not run.
    fn try_invoke(&self, h: i32) -> HResult<Option<Wasm>> {
        let outer = self.trying.replace(true);
        let invoked = self.invoke(h);
        self.trying.set(outer);
        let Some(Wasm::I32(raw)) = invoked? else {
            return trap("invoke returned no handle");
        };
        let outcome = unboxed(self.val_whole(raw)?);
        let mut fields = std::collections::BTreeMap::new();
        let variant = match outcome {
            Value::Error(reason) => {
                fields.insert(tilolang::RESULT_ERROR.to_owned(), Value::Text(reason));
                tilolang::RESULT_ERR
            }
            Value::Unit => tilolang::RESULT_OK,
            v => {
                fields.insert(tilolang::RESULT_VALUE.to_owned(), v);
                tilolang::RESULT_OK
            }
        };
        self.ret_h(Value::Enum(crate::value::EnumValue {
            ty: tilolang::RESULT_TYPE.to_owned(),
            variant: variant.to_owned(),
            fields,
        }))
    }

    fn model_call(&self, kind_h: i32, input_h: i32) -> HResult<Option<Wasm>> {
        let (kind, input) = (self.text(kind_h)?, unboxed(self.val_whole(input_h)?));
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("model_call is only available to an action's code");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Model { kind: k, result } = entry else {
                    return trap("the run diverged from its journal: expected a model call");
                };
                if *k != kind {
                    return trap(format!(
                        "the run diverged from its journal: model call `{k}`"
                    ));
                }
                let result = result.clone();
                run.cursor = run.cursor.saturating_add(1);
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
        self.queue_outside(cap_h, payload_h, None)
    }

    /// `outside(capability, payload, at: instant)`: the effect, held back until `instant`, the
    /// time `now` is a moment of (`2026-06-30T13:40Z`), ISO-8601 UTC text with the seconds.
    fn outside_at(&self, cap_h: i32, payload_h: i32, at_h: i32) -> HResult<Option<Wasm>> {
        self.queue_outside(cap_h, payload_h, Some(at_h))
    }

    fn queue_outside(
        &self,
        cap_h: i32,
        payload_h: i32,
        held: Option<i32>,
    ) -> HResult<Option<Wasm>> {
        let capability = self.text(cap_h)?;
        let payload = outside_json(&unboxed(self.val_whole(payload_h)?))?;
        if !tilolang::origin::is_capability(&capability) {
            return trap(format!("unknown outside capability `{capability}`"));
        }
        let held_until = match held {
            Some(h) => {
                let at = self.text(h)?;
                tilolang::calendar::instant(&at).ok_or_else(|| {
                    HostFail::Trap(format!("`outside(.., at: {at})`: that is not a time"))
                })?;
                Some(at)
            }
            None => None,
        };
        let (txn, action, primary, key) = {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("outside is only available in actions");
            };
            // One key per effect of the run the action belongs to: a device that runs the same
            // invocation, before or after a person was asked, queues each effect under the key
            // the device that ran it first did, so the record holds it once. An effect the
            // journal answers was counted when the run first came this way, and counting it
            // again would hold the effects after it under keys no other device uses.
            let key = (!run.replaying()).then(|| run.next_effect_key());
            let (txn, action, primary) = match &frames.top()?.kind {
                FrameKind::Action(a) => (a.txn, a.action.clone(), a.primary.clone()),
                _ => return trap("outside is only available in actions"),
            };
            (txn, action, primary, key)
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
        self.journaled_apply(
            txn,
            &primary,
            &Mutation::OutboxPush {
                capability,
                payload,
                held_until,
                key,
            },
        )?;
        self.ret_h(Value::Unit)
    }

    /// `withdraw(r)`: take back the outside effects `r`'s actions queued that nothing carried
    /// out yet, so they never happen. Only an action asks for it, of its own subject, or the
    /// undo of one: what a person wrote and took back before it went.
    fn withdraw(&self, primary_h: i32, reason_h: i32) -> HResult<Option<Wasm>> {
        let primary = self.ref_arg(primary_h)?;
        let reason = self.text(reason_h)?;
        let (invoker, chain) = {
            let frames = self.frames.borrow();
            match &frames.top()?.kind {
                FrameKind::Action(a) => (a.invoker.clone(), a.action.clone()),
                _ => return trap("withdraw is only available in actions"),
            }
        };
        let _ = chain;
        let note = ChangeNote {
            action: "$withdraw".to_owned(),
            args: Json::String(reason.clone()),
            invoker: invoker.0,
            ..ChangeNote::default()
        };
        self.store
            .borrow_mut()
            .outbox_withdraw(&primary.target.doc, &reason, &note)
            .map_err(|e| HostFail::Trap(e.to_string()))?;
        self.ret_h(Value::Unit)
    }

    fn act_ref(&self, id_h: i32, args_h: i32) -> HResult<Option<Wasm>> {
        let action = self.text(id_h)?;
        let Value::List(args) = self.val(args_h)? else {
            return trap("act_ref expects the arguments as a list");
        };
        // How many arguments it takes, and how many of the last of them an application may
        // leave out: those with defaults, computed when it is invoked, and those its form asks
        // the person for (forms-and-diagrams.md §1).
        let arity = self
            .registry
            .borrow()
            .action_package(&action)
            .and_then(|pkg| {
                pkg.action(&action).map(|a| {
                    let defaulted = a
                        .params
                        .iter()
                        .rev()
                        .take_while(|p| {
                            p.default.is_some() || crate::platform::form_asks(&pkg.meta, &p.ty)
                        })
                        .count();
                    (a.params.len(), defaulted)
                })
            });
        match arity {
            None => return trap(format!("unknown action `{action}`")),
            Some((n, defaulted)) if args.len() > n || args.len() < n.saturating_sub(defaulted) => {
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
            from: None,
        }))
    }

    pub(crate) fn principal_arg(&self, h: i32) -> HResult<Principal> {
        self.with_val(h, |v| match v {
            Value::Principal(p) => Ok(p.clone()),
            other => trap(format!("expected a principal, got {}", other.kind_name())),
        })
    }

    fn queue_block(&self, part_h: i32, field_h: i32, source_h: i32) -> HResult<Option<Wasm>> {
        let part = Rc::unwrap_or_clone(self.ref_arg(part_h)?.target);
        let (field, source) = (self.text(field_h)?, self.text(source_h)?);
        let known = self
            .registry
            .borrow()
            .type_info(&part.ty)
            .is_some_and(|(_, info)| info.field(&field).is_some());
        if !known {
            return trap(format!("`{}` has no field `{field}`", part.ty));
        }
        let mut frames = self.frames.borrow_mut();
        let FrameKind::Action(state) = &mut frames.top_mut()?.kind else {
            return trap("run_block is only available in actions");
        };
        state.queued.push(QueuedBlock {
            part,
            field,
            source,
        });
        ret_void()
    }

    /// The clock and the person's zone, as the code running reads them: what the run read the
    /// first time, from its journal when it recorded one then, else from the host, which it
    /// records. Code that is no run (a view's) reads the host live.
    pub(crate) fn clock(&self) -> HResult<crate::run::Clock> {
        // Code that is no run reads the host live: the frames are let go first, because
        // reading the host asks them what the run it belongs to reads.
        let in_a_run = { self.frames.borrow_mut().run_mut().is_some() };
        if !in_a_run {
            return Ok(self.host_clock());
        }
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return Ok(self.host_clock());
            };
            if let Some(clock) = &run.clock {
                return Ok(clock.clone());
            }
            match run.next_entry() {
                Some(JournalEntry::Clock(clock)) => {
                    run.clock = Some(clock.clone());
                    return Ok(clock);
                }
                Some(_) => {
                    return trap("the run diverged from its journal: expected the clock's entry");
                }
                None => {}
            }
        }
        let clock = self.host_clock();
        if let Some(run) = self.frames.borrow_mut().run_mut() {
            run.clock = Some(clock.clone());
            run.push(JournalEntry::Clock(clock.clone()));
        }
        Ok(clock)
    }

    /// What the host says now: its clock, and the person's zone.
    fn host_clock(&self) -> crate::run::Clock {
        crate::run::Clock {
            now: self.invocation_time(),
            zone: self.zone_name.borrow().as_ref().and_then(|name| name()),
            offset: self.zone.borrow().as_ref().map_or(0, |zone| zone()),
        }
    }

    /// The clock and zone the code running read, when it read time.
    pub(crate) fn pinned_clock(&self) -> Option<crate::run::Clock> {
        self.frames.borrow().run().and_then(|run| run.clock.clone())
    }

    /// The moment the change being made is recorded at: the one the action read, when it read
    /// the clock, else the host's, which nothing replays.
    fn change_time(&self) -> String {
        self.invocation_time()
    }

    /// A moment moved by some seconds, backwards when `back` says.
    fn time_shift(&self, at_h: i32, secs: i64, back_h: i32) -> HResult<Option<Wasm>> {
        let at = self.text(at_h)?;

        // `back` crosses as a plain `i32`, not a handle: 1 backwards, 0 forwards.
        let back = back_h != 0;
        let Some(instant) = tilolang::calendar::instant(&at) else {
            return trap(format!("`{at}` is not a time"));
        };
        let moved = if back {
            instant.saturating_sub(secs)
        } else {
            instant.saturating_add(secs)
        };
        self.ret_h(Value::Text(tilolang::calendar::time_text(moved)))
    }

    fn now(&self) -> HResult<Option<Wasm>> {
        let time = self.now_text()?;
        tilolang::types::check_time(&time)
            .map_err(|e| HostFail::Trap(format!("the host's clock: {e}")))?;
        self.ret_h(Value::Text(time))
    }

    /// What this invocation reads as `now`: the moment a scenario acts at, else the moment it
    /// first read the clock, which a resumed run answers from its journal. `today` is the day it
    /// falls on, so the two never disagree.
    pub(crate) fn now_text(&self) -> HResult<String> {
        {
            let frames = self.frames.borrow();
            if let FrameKind::Action(state) = &frames.top()?.kind
                && let Some(time) = state.invocations.last().and_then(|i| i.at.clone())
            {
                return Ok(time);
            }
        }
        Ok(self.clock()?.now)
    }

    /// The time of the outermost invocation running: the moment it recorded, else the host's
    /// clock at its first use, kept until it returns.
    fn invocation_time(&self) -> String {
        if let Some(clock) = self.pinned_clock() {
            return clock.now;
        }
        if let Some(time) = self.frames.borrow().now.clone() {
            return time;
        }
        let time = self.clock_now();
        self.frames.borrow_mut().now = Some(time.clone());
        time
    }

    /// In a scenario, commit what the running transaction did so far as one change, made by
    /// whoever the scenario runs as now and at the time it runs at, and go on in a new one: each
    /// `invoke` is a change of its own, which history and reflection show as that person's at
    /// that time.
    fn scenario_segment(&self) -> HResult<()> {
        let at = self.change_time();
        let (txn, note) = {
            let mut frames = self.frames.borrow_mut();
            let FrameKind::Action(state) = &mut frames.top_mut()?.kind else {
                return trap("only a scenario names people and acts as them");
            };
            if !state.scenario {
                return trap("only a scenario names people and acts as them");
            }
            let mut note = (*state.note).clone();
            match state.invocations.last() {
                Some(inv) => {
                    note.invoker.clone_from(&inv.who.0);
                    note.at = Some(inv.at.clone().unwrap_or(at));
                    note.from = inv.from.clone().or_else(|| state.note.from.clone());
                }
                // At the moment the invocation named, when it named one; else now.
                None => note.at = note.at.clone().or(Some(at)),
            }
            note.touched = std::mem::take(&mut state.touched).into_iter().collect();
            (state.txn, note)
        };
        let next = {
            let mut store = self.store.borrow_mut();
            store
                .commit(txn, &note)
                .map_err(|e| HostFail::Trap(e.to_string()))?;
            store.begin().map_err(|e| HostFail::Trap(e.to_string()))?
        };
        if let FrameKind::Action(state) = &mut self.frames.borrow_mut().top_mut()?.kind {
            state.txn = next;
        }
        Ok(())
    }

    /// `suggest act(args)`: records the action as a proposal attached to the resource it is
    /// about, instead of applying it. An action records it once it has committed, so the
    /// proposal is never part of its transaction; a program holds no transaction of its own, so
    /// it records it now.
    fn propose_in_frame(&self, act: i32) -> HResult<Option<Wasm>> {
        let aref = match self.val(act)? {
            Value::Action(aref) => aref,
            other => return trap(format!("expected an action, got {}", other.kind_name())),
        };
        let now = {
            let mut frames = self.frames.borrow_mut();
            match &mut frames.top_mut()?.kind {
                FrameKind::Action(state) => {
                    let who = state
                        .invocations
                        .last()
                        .map_or_else(|| state.invoker.clone(), |inv| inv.who.clone());
                    state.suggestions.push((who, aref.clone()));
                    None
                }
                FrameKind::Program(state) => Some(state.invoker.clone()),
                _ => return trap("only an action or a program may suggest an action"),
            }
        };
        if let Some(who) = now {
            self.propose(&who, &aref)
                .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        }
        ret_void()
    }

    /// Run `f` on the top frame's action state, which must be a scenario's: only seed data
    /// names people and acts as them.
    fn scenario_state(&self, f: impl FnOnce(&mut ActionState)) -> HResult<()> {
        let mut frames = self.frames.borrow_mut();
        match &mut frames.top_mut()?.kind {
            FrameKind::Action(state) if state.scenario => {
                f(state);
                Ok(())
            }
            _ => trap("only a scenario names people and acts as them"),
        }
    }

    /// `web.get`, `web.post` and what a reply is asked: its status, headers and body.
    pub(crate) fn web_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::WebGet => {
                let url = self.text(a.h(0)?)?;
                let headers = self.web_headers(a.h(1)?)?;
                let if_none_match = match self.val(a.h(2)?)? {
                    Value::Opt(Some(tag)) => Some(match *tag {
                        Value::Text(tag) => tag,
                        other => {
                            return trap(format!(
                                "if_none_match takes text, got {}",
                                other.kind_name()
                            ));
                        }
                    }),
                    Value::Opt(None) => None,
                    other => {
                        return trap(format!(
                            "if_none_match takes text, got {}",
                            other.kind_name()
                        ));
                    }
                };
                let request = crate::run::WebRequest {
                    method: "GET".to_owned(),
                    url,
                    headers,
                    body: None,
                    if_none_match,
                };
                let response = self.web(request)?;
                self.ret_h(Value::Response(Rc::new(response)))
            }
            Imp::WebPost => {
                let url = self.text(a.h(0)?)?;
                let body = self.text(a.h(1)?)?;
                let headers = self.web_headers(a.h(2)?)?;
                let request = crate::run::WebRequest {
                    method: "POST".to_owned(),
                    url,
                    headers,
                    body: Some(body),
                    if_none_match: None,
                };
                let response = self.web(request)?;
                self.ret_h(Value::Response(Rc::new(response)))
            }
            Imp::WebOk => {
                let response = self.response_arg(a.h(0)?)?;
                ret_i32(i32::from(response.ok()))
            }
            Imp::WebStatus => {
                let response = self.response_arg(a.h(0)?)?;
                crate::core::ret_i64(i64::from(response.status))
            }
            Imp::WebStatusText => {
                let response = self.response_arg(a.h(0)?)?;
                self.ret_h(Value::text(response.status_text.clone()))
            }
            Imp::WebHeaders => {
                let response = self.response_arg(a.h(0)?)?;
                let headers = response
                    .headers
                    .iter()
                    .map(|line| Value::text(line.clone()))
                    .collect();
                self.ret_h(Value::List(headers))
            }
            Imp::WebBody => {
                let response = self.response_arg(a.h(0)?)?;
                let bytes = Rc::new(response.bytes());
                self.ret_h(Value::Bytes(bytes))
            }
            Imp::WebText => {
                let text = match self.val(a.h(0)?)? {
                    Value::Response(response) => {
                        String::from_utf8_lossy(&response.bytes()).into_owned()
                    }
                    Value::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    other => {
                        return trap(format!("expected bytes, got {}", other.kind_name()));
                    }
                };
                self.ret_h(Value::Text(text))
            }
            Imp::BytesLength => {
                let n = match self.val(a.h(0)?)? {
                    Value::Bytes(bytes) => bytes.len(),
                    other => {
                        return trap(format!("expected bytes, got {}", other.kind_name()));
                    }
                };
                crate::core::ret_i64(i64::try_from(n).unwrap_or(i64::MAX))
            }
            _ => trap("not a web import"),
        }
    }

    /// The headers a call was given: each `Name: value`.
    fn web_headers(&self, h: i32) -> HResult<Vec<String>> {
        match self.val(h)? {
            Value::List(items) => items
                .iter()
                .map(|item| match item {
                    Value::Text(line) => Ok(line.clone()),
                    other => trap(format!(
                        "headers take text like `Name: value`, got {}",
                        other.kind_name()
                    )),
                })
                .collect(),
            other => trap(format!("headers take a list, got {}", other.kind_name())),
        }
    }

    fn response_arg(&self, h: i32) -> HResult<Rc<crate::run::WebResponse>> {
        self.with_val(h, |v| match v {
            Value::Response(response) => Ok(Rc::clone(response)),
            other => trap(format!("expected a response, got {}", other.kind_name())),
        })
    }

    /// Fetch `request` for the run running, once: its journal answers a request it made before,
    /// else the transport does and the answer is recorded, so a run that replays makes each
    /// request once. A host the package did not declare, one the workspace revoked, and any
    /// network failure are values: the run carries on and sees why.
    fn web(&self, request: crate::run::WebRequest) -> HResult<crate::run::WebResponse> {
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                return trap("a request is only made in an action");
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Web {
                    request: before,
                    response,
                } = entry
                else {
                    return trap(
                        "the run diverged from its journal: expected a web request's entry",
                    );
                };
                if *before != request {
                    return trap(format!(
                        "the run diverged from its journal: it now asks for {} {}, not {} {}",
                        request.method, request.url, before.method, before.url
                    ));
                }
                let response = response.clone();
                run.cursor = run.cursor.saturating_add(1);
                return Ok(response);
            }
        }
        let response = self.web_response(&request);
        self.journal_push(JournalEntry::Web {
            request,
            response: response.clone(),
        })?;
        Ok(response)
    }

    /// The answer `request` gets now: the transport's, or why it was not made.
    fn web_response(&self, request: &crate::run::WebRequest) -> crate::run::WebResponse {
        let Ok(module) = self.module_of_frame() else {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(
                "web is only available in actions",
            ));
        };
        let package = self.registry.borrow().package(&module);
        let Some(package) = package else {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(format!(
                "no package wrote the code at {}",
                module.0
            )));
        };
        let app = package.meta.app.clone();
        let Some(host) = crate::web::host_of(&request.url) else {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(
                "only http and https requests are made",
            ));
        };
        if !crate::web::granted(&package.meta.web, &host) {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(format!(
                "`{app}` may not reach `{host}`: it declares web: [{}]",
                package.meta.web.join(", ")
            )));
        }
        if !self.authority.may_fetch(&app, &host) {
            return crate::run::WebResponse::of(crate::web::WebReply::failed(format!(
                "the workspace revoked `{host}` for `{app}`"
            )));
        }
        let reply = self.web.borrow().as_ref().map_or_else(
            || crate::web::WebReply::failed("this host reaches no services"),
            |transport| transport.fetch(request),
        );
        crate::run::WebResponse::of(reply)
    }

    /// `notify(to, title, body, opens)`: tell the person `to` something, once. The notice is a
    /// record written in this action's own change ([`crate::notify`]), so every device of theirs
    /// reads it. The run's journal answers a run that replays, so it tells them no more than the
    /// first run did.
    fn notify_import(&self, to: i32, title: i32, body: i32, opens: i32) -> HResult<Option<Wasm>> {
        let to = self.principal_arg(to)?;
        let title = self.text(title)?;
        let body = self.text(body)?;
        let opens = match self.val(opens)? {
            Value::Ref(r) => ResourceRef::clone(&r.target),
            other => {
                return trap(format!(
                    "notify: expected the resource it opens, got {}",
                    other.kind_name()
                ));
            }
        };
        let notice = crate::notify::Notice {
            to,
            title,
            body,
            opens,
        };
        let (txn, id) = {
            let mut frames = self.frames.borrow_mut();
            let id = {
                let Some(run) = frames.run_mut() else {
                    return trap("a notification is only sent in an action");
                };
                if let Some(entry) = run.journal.entries.get(run.cursor) {
                    if !matches!(entry, crate::run::JournalEntry::Notified) {
                        return trap(
                            "the run diverged from its journal: expected a notification's entry",
                        );
                    }
                    run.cursor = run.cursor.saturating_add(1);
                    return ret_void();
                }
                // The notice is an outside effect of the run like any other (`outside`), and
                // the record it is takes that effect's key for its id: a replica that runs the
                // invocation again — one that holds no journal of the run, before or after a
                // person was asked — writes the one record, so the person is told once.
                crate::notify::record_id(&run.next_effect_key())
            };
            let txn = match &frames.top()?.kind {
                FrameKind::Action(state) => state.txn,
                _ => return trap("a notification is only sent in an action"),
            };
            (txn, id)
        };
        self.write_notice(txn, &id, &notice)?;
        self.journal_push(crate::run::JournalEntry::Notified)?;
        ret_void()
    }

    /// Write the notice `notice` as the record `id` of this action's change, in the transaction
    /// `txn`: creating a resource writes nothing of the app's own, so the change takes its app
    /// and kind from what else the action wrote. A record already under `id` is that notice,
    /// written by a replica of the run before this one: it is not written again.
    fn write_notice(
        &self,
        txn: crate::store::Txn,
        id: &str,
        notice: &crate::notify::Notice,
    ) -> HResult<()> {
        use tilolang::{NOTICE_BODY, NOTICE_OPENS, NOTICE_TITLE, NOTICE_TO, NOTICE_TYPE};

        let record = self
            .registry
            .borrow()
            .new_record(NOTICE_TYPE)
            .ok_or_else(|| HostFail::Trap("the host keeps no notice type".into()))?;
        let mut store = self.store.borrow_mut();
        let at = ResourceRef::root(id.to_owned(), NOTICE_TYPE.to_owned());
        if store.type_of(&at).is_ok() {
            return Ok(());
        }
        let at = store
            .create_in(txn, &record, id)
            .map_err(|error| HostFail::Trap(error.to_string()))?;
        let scalar = |value: &Value, ty: &crate::meta::Ty| {
            crate::host_res::to_scalar(value, ty)
                .map_err(|error| HostFail::Trap(format!("notify: {error}")))
        };
        let writes = [
            (
                NOTICE_TO.to_owned(),
                crate::store::Mutation::SetRegister {
                    field: NOTICE_TO.to_owned(),
                    value: scalar(
                        &Value::Principal(notice.to.clone()),
                        &crate::meta::Ty::Principal,
                    )?,
                },
            ),
            (
                NOTICE_TITLE.to_owned(),
                crate::store::Mutation::SetText {
                    field: NOTICE_TITLE.to_owned(),
                    text: notice.title.clone(),
                },
            ),
            (
                NOTICE_BODY.to_owned(),
                crate::store::Mutation::SetText {
                    field: NOTICE_BODY.to_owned(),
                    text: notice.body.clone(),
                },
            ),
            (
                NOTICE_OPENS.to_owned(),
                crate::store::Mutation::SetRegister {
                    field: NOTICE_OPENS.to_owned(),
                    value: scalar(
                        &Value::resource(&notice.opens),
                        &crate::meta::Ty::Record(tilolang::resource_key()),
                    )?,
                },
            ),
        ];
        for (field, write) in writes {
            store
                .apply(txn, &at, &write)
                .map_err(|error| HostFail::Trap(format!("notify: {field}: {error}")))?;
        }
        Ok(())
    }

    pub(crate) fn effect_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::ActRef => self.act_ref(a.h(0)?, a.h(1)?),
            Imp::Invoke => self.invoke(a.h(0)?),
            Imp::TryInvoke => self.try_invoke(a.h(0)?),
            Imp::Suggest => self.propose_in_frame(a.h(0)?),
            Imp::ProposalNote => {
                let note = self.proposal_note(a.h(0)?)?;
                self.ret_h(note)
            }
            Imp::CallFn => self.fn_call(a.h(0)?, a.h(1)?),
            Imp::IsError => {
                let failed = self.with_val(a.h(0)?, |v| Ok(matches!(v, Value::Error(_))))?;
                ret_i32(i32::from(failed))
            }
            Imp::Outside => self.outside(a.h(0)?, a.h(1)?),
            Imp::OutsideAt => self.outside_at(a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::Withdraw => self.withdraw(a.h(0)?, a.h(1)?),
            Imp::ModelCall => self.model_call(a.h(0)?, a.h(1)?),
            Imp::Notify => self.notify_import(a.h(0)?, a.h(1)?, a.h(2)?, a.h(3)?),
            Imp::Invoker => {
                let frames = self.frames.borrow();
                let who = match &frames.top()?.kind {
                    FrameKind::Action(state) => state
                        .invocations
                        .last()
                        .map_or(&state.invoker, |inv| &inv.who),
                    // An action's requirement read where it is presented: the person looking,
                    // who would invoke it.
                    FrameKind::View(view) => &view.viewer,
                    _ => return trap("invoker is only available in actions"),
                };
                let who = Value::Principal(who.clone());
                drop(frames);
                self.ret_h(who)
            }
            Imp::PrincipalNamed | Imp::AgentNamed => {
                let name = self.text(a.h(0)?)?;
                self.scenario_state(|_| ())?;
                let who = Principal(format!("scenario:{name}"));
                if imp == Imp::AgentNamed {
                    self.named_agent(who.clone());
                } else {
                    self.named_person(who.clone());
                }
                self.ret_h(Value::Principal(who))
            }
            Imp::InvocationPush => {
                let who = self.principal_arg(a.h(0)?)?;
                let at = match self.val_whole(a.h(1)?)? {
                    Value::Opt(Some(time)) => match *time {
                        Value::Text(t) => Some(t),
                        other => {
                            return trap(format!("expected a Time, got {}", other.kind_name()));
                        }
                    },
                    Value::Opt(None) => None,
                    other => {
                        return trap(format!("expected a Time, got {}", other.kind_name()));
                    }
                };
                let from = match self.val_whole(a.h(2)?)? {
                    Value::Opt(Some(source)) => match *source {
                        Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                        other => {
                            return trap(format!("expected a resource, got {}", other.kind_name()));
                        }
                    },
                    Value::Opt(None) => None,
                    other => {
                        return trap(format!("expected a resource, got {}", other.kind_name()));
                    }
                };
                self.scenario_segment()?;
                let invocation = Invocation { who, at, from };
                self.scenario_state(|state| state.invocations.push(invocation))?;
                ret_void()
            }
            Imp::InvocationPop => {
                self.scenario_segment()?;
                self.scenario_state(|state| {
                    state.invocations.pop();
                })?;
                ret_void()
            }
            Imp::PrincipalEq => {
                let (x, y) = (self.principal_arg(a.h(0)?)?, self.principal_arg(a.h(1)?)?);
                ret_i32(i32::from(x == y))
            }
            Imp::PrincipalName => {
                let p = self.principal_arg(a.h(0)?)?;
                let name = self
                    .names
                    .borrow()
                    .as_ref()
                    .map_or_else(|| p.0.clone(), |host| host.name(&p));
                self.ret_h(Value::Text(name))
            }
            Imp::QueueBlock => self.queue_block(a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::Now => self.now(),
            Imp::TimeShift => self.time_shift(a.h(0)?, a.long(1)?, a.h(2)?),
            _ => trap("not an effect import"),
        }
    }
}

/// A payload as the outside world reads it: plain JSON, with lists and tuples as arrays and
/// a resource as `{ref, type}`, rather than the tagged form journals and registers store.
fn outside_json(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::List(items) | Value::Tuple(items) => Json::Array(
            items
                .iter()
                .map(outside_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Opt(Some(inner)) => outside_json(inner)?,
        Value::Opt(None) => Json::Null,
        Value::Ref(r) => serde_json::json!({
            "ref": r.target.id_string(),
            "type": r.target.ty,
        }),
        other => to_json(other)?,
    })
}

fn entry_value(entry: &JournalEntry) -> Value {
    match entry {
        JournalEntry::Invoked { result, .. } | JournalEntry::Model { result, .. } => result.clone(),
        JournalEntry::Read { value } => value.clone(),
        JournalEntry::Denied { reason, .. } | JournalEntry::Failed { reason, .. } => {
            Value::Error(reason.clone())
        }
        // A read of the clock, a draw and a fetch answer the code that made them where they
        // are answered; none of them is a step's value.
        JournalEntry::Clock(_) | JournalEntry::Random { .. } | JournalEntry::Web { .. } => {
            Value::Unit
        }
        // A write is answered where it is made, and a run that waited answers it there; a
        // notification is no step's value either.
        JournalEntry::Wrote { .. } | JournalEntry::Notified => Value::Unit,
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
            "the run diverged from its journal: it now invokes `{action}`"
        )),
    }
}
