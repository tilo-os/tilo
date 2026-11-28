//! Invocation lifecycles.
//!
//! * **An action** runs in one store transaction. Its writes go into the transaction; if it
//!   returns, the store commits one change per touched document, each carrying a
//!   [`ChangeNote`] (the action, its arguments, the invoker, the authorship chain and the
//!   module). If it traps or runs out of fuel or time, nothing is committed.
//! * **An action that calls another action** is a run: a journal ([`crate::run`]). A call goes
//!   through the action API: the authority decides it, and it runs as a change of its own. The
//!   caller's transaction ends at the call, and it runs again from its journal, which answers
//!   the call and everything it read before it. An invocation the authority or a person must
//!   answer first suspends the run; the answer runs it on from the same journal, so nothing runs
//!   twice.
//! * **An agent's program** is the same run without a transaction: every change it makes goes
//!   through the host's invoker, to the app that holds the records.
//!
//! An action's arguments left out are completed from their defaults, on the invoker's state,
//! before it is decided, journaled or run. An action's `outside` call doesn't reach outside
//! while the action runs: it is queued in the action's change as an effect entry, which the
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
use crate::ids::{Actor, AppId, ModuleHash, RunId};
use crate::meta::{ActionMeta, EffectClass, ParamMeta, Repr, Ty, parse_ty};
use crate::registry::Package;
use crate::run::{Journal, JournalEntry, Pending, Press, RunOutcome, Step};
use crate::store::{Applied, ChangeNote, Mutation, Txn};
use crate::value::{ActionRef, ResourceRef, Value, to_json};
use tilolang::types::Confirms;

mod page;
mod web;

/// Calls into other packages' `fn`s deeper than this are refused: each one is a fresh instance
/// of the callee's module, running on the host's stack, so recursion across packages would
/// otherwise exhaust it.
const MAX_CALL_DEPTH: usize = 32;

/// Most addresses one question names, over all its entries shown as addresses. A call that names
/// more is refused before its person is asked, with how many it names, rather than shown cut:
/// what they allow is what they read.
const MOST_SHOWN_ADDRESSES: usize = 20;

/// The longest address a question shows whole.
const MOST_SHOWN_ADDRESS_CHARS: usize = 100;

/// Most characters of a text entry a question shows before counting the rest.
const MOST_SHOWN_TEXT_CHARS: usize = 200;

/// Most characters of a long text entry (a message's body) a question shows before counting the
/// rest.
const MOST_SHOWN_LONG_CHARS: usize = 600;

/// How many addresses `value`, an entry shown as addresses, names: each part between commas,
/// semicolons or line breaks that is not blank.
fn addresses_in(value: &str) -> usize {
    named_addresses(value).count()
}

/// Each address `value`, an entry shown as addresses, names: each part between commas,
/// semicolons or line breaks that is not blank, trimmed.
fn named_addresses(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

/// Whether a person reads `named` for the address it is: the address itself (what stands between
/// `<` and `>` where a name goes with it) is plain ASCII with no quoted part and no space, and no
/// longer than a question shows whole. A letter outside ASCII can stand for another that looks
/// the same, so what they allowed would not be what they read.
fn reads_plainly(named: &str) -> bool {
    let address = match (named.rfind('<'), named.rfind('>')) {
        (Some(open), Some(close)) if open < close => named.get(open.saturating_add(1)..close),
        _ => Some(named),
    }
    .unwrap_or(named);
    !address.is_empty()
        && address.chars().count() <= MOST_SHOWN_ADDRESS_CHARS
        && address.chars().all(|c| c.is_ascii_graphic() && c != '"')
}

/// Who an invocation runs for, and through which code.
#[derive(Clone, Debug)]
pub struct ExecCtx {
    pub invoker: Actor,
    /// The app whose process the invoker calls through, when the action API admitted the call from
    /// a process the device started for that app: what `invoker.app()` reads.
    pub app: Option<String>,
    pub chain: Vec<ModuleHash>,
    pub approved_by: Option<Actor>,
    /// The run this invocation already is, when something every replica reads names it (a
    /// trigger, run for a record at an instant), and a device that holds no journal of it runs
    /// its action under that run. Every effect the
    /// run queues is keyed by the id it starts with, so a device that runs the same invocation
    /// queues each effect once. Without one, the run gets an id of its own.
    pub identity: Option<RunId>,
    /// The resource the invocation was made from, when it was made from one: what the resources
    /// it creates were made from, recorded in the change ([`crate::store::Version::from`]).
    pub from: Option<ResourceRef>,
    /// The moment the invocation acts at, when a scenario named one: what its clock reads.
    pub at: Option<String>,
    /// The gesture every change of the invocation is one of, when the person's act made more than
    /// one change (a picture pasted into a document, and the app naming it): undo takes them back
    /// as one step ([`crate::store::ChangeNote::gesture`]).
    pub gesture: Option<String>,
}

impl ExecCtx {
    /// The journal of a run of `aref` for this invocation, belonging to `invocation`, before it
    /// did anything.
    fn journal(&self, aref: &ActionRef, invocation: RunId) -> Journal {
        Journal {
            action: Some(aref.action.clone()),
            args: aref.args.clone(),
            invoker: self.invoker.clone(),
            app: self.app.clone(),
            chain: self.chain.clone(),
            from: self.from.clone(),
            at: self.at.clone(),
            entries: Vec::new(),
            pending: None,
            finished: false,
            approved_by: self.approved_by.clone(),
            asked: false,
            pressed: None,
            effects: 0,
            parent: None,
            served: None,
            invocation,
            invoked: String::new(),
        }
    }
}

/// How an action ended.
#[derive(Debug)]
pub enum ActionOutcome {
    Done(Value),
    /// It waits: on a person's answer, or on another app's ([`RunOutcome`]).
    Waits(RunOutcome),
    /// The action trapped or ran out of fuel or time; its change was rolled back.
    Failed(String),
}

/// How a direct invocation ended.
#[derive(Clone, Debug, PartialEq)]
pub enum InvokeOutcome {
    Done(Value),
    /// It waits on the answer to the call `call` it made through the dispatch, as the run `run`:
    /// another app's action, or one a person is asked about first.
    Waits {
        run: RunId,
        call: String,
    },
    /// It needs a person's yes first: the call `call` went through the dispatch asking them, and
    /// the app runs it when they answer.
    Asked(String),
    Denied,
}

impl From<RunOutcome> for InvokeOutcome {
    fn from(outcome: RunOutcome) -> Self {
        match outcome {
            RunOutcome::Done(value) => Self::Done(value),
            RunOutcome::Waits { run, call } => Self::Waits { run, call },
        }
    }
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
    /// The code trapped, ran out of fuel or time, or could not go on: what it did is rolled
    /// back.
    Failed {
        journal: Journal,
        error: RuntimeError,
    },
}

/// What an invocation the host sends on came to.
enum Delegated {
    /// It ran, or was refused or failed: what the run journals of it.
    Ran(Box<JournalEntry>),
    /// It went to its app, which answers the call `call` later.
    Waits(String),
}

/// The change an action's completed attempt makes.
struct Change {
    txn: Txn,
    queued: Vec<QueuedBlock>,
    suggested: Vec<(Actor, ActionRef)>,
    note: ChangeNote,
}

/// How a run whose code stopped came to be run again for its answer.
enum RunEnd {
    Done {
        journal: Journal,
        value: Value,
    },
    /// It waits on a call ([`RunOutcome::Waits`]).
    Waits(RunOutcome),
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
/// the request, a store that could not hold it, or a scenario that stopped at a step it may not
/// take (which keeps what it wrote before): what a caller sees as a failed invocation.
fn failed(e: &RuntimeError) -> bool {
    !matches!(
        e,
        RuntimeError::BadArgs(_)
            | RuntimeError::Store(_)
            | RuntimeError::NotFound(_)
            | RuntimeError::NotPending(_)
            | RuntimeError::Stopped(_)
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
        want == r.target.ty || crate::meta::foreign_type(&r.target.ty).as_ref() == Some(key)
    }

    /// The value a parameter of type `ty` holds when `v` is given for it: a handle-represented
    /// value of an option's own type given without its box is that option's `some`, as a caller
    /// outside the language names the thing it passes (a resource reference for
    /// `Option<Project>`) where the code reads an option. A record value is no such thing: an
    /// action handed one reads it and cannot change it, and a copy inside an option says
    /// nothing of that, so it is still refused. Anything else is the value as it is, and
    /// [`Self::check_param`] says when it does not fit.
    fn param_value(&self, ty: &Ty, v: &Value, app: &str) -> Result<Value, String> {
        if let Ty::Option(inner) = ty
            && inner.abi_repr() == Some(Repr::Handle)
            && !matches!(v, Value::Opt(_) | Value::Rec(_))
        {
            self.check_param(inner, v, app).map_err(|_| {
                format!(
                    "a {} does not fit a parameter of type {}",
                    v.kind_name(),
                    ty.display(app)
                )
            })?;
            return Ok(Value::some(v.clone()));
        }
        self.check_param(ty, v, app)?;
        Ok(v.clone())
    }

    /// Check a handle-represented argument before allocating it in the call frame.
    /// Called only from the `Repr::Handle` arm of [`Self::marshal`].
    fn check_param(&self, ty: &Ty, v: &Value, app: &str) -> Result<(), String> {
        debug_assert_eq!(ty.abi_repr(), Some(Repr::Handle));
        let ok = match (ty, v) {
            // A program's `host.Resource` is any resource.
            (Ty::Record(key), Value::Ref(_)) if *key == tilolang::resource_key() => true,
            (Ty::Record(key), Value::Ref(r)) => self.ref_fits(key, r),
            // A record value (`DockItem(...)` built in a view, a record `json.decode` read, one
            // taken out of a list of them) is a value of its declared type wherever that type's
            // parameter takes it: a function's, a view's, or an action's, which reads it. It
            // names no resource, so an action's write to it fails where it is written rather
            // than landing in a copy that is dropped.
            (Ty::Record(key), Value::Rec(rec)) => {
                let registry = self.registry.borrow();
                if registry.type_for(key).as_deref() == Some(rec.ty.as_str()) {
                    true
                } else {
                    let held = registry
                        .type_info(&rec.ty)
                        .map_or_else(|| rec.ty.clone(), |(_, info)| info.name.clone());
                    return Err(format!(
                        "a record of `{held}` is no `{}`: a record value is a value of its own type",
                        ty.display(app)
                    ));
                }
            }
            // A value of one of a union's types stands where the union's parameter does.
            (Ty::Union(keys), Value::Rec(rec)) => {
                let registry = self.registry.borrow();
                keys.iter()
                    .any(|key| registry.type_for(key).as_deref() == Some(rec.ty.as_str()))
            }
            (Ty::Union(keys), Value::Ref(r)) => keys.iter().any(|key| self.ref_fits(key, r)),
            // A host value (an action, a lens, a type's metadata) is held as its fields, and so
            // is a filter, held as the `host.Filter` its catalogue entry is.
            (Ty::Record(key), Value::Tuple(_)) => tilolang::host::is_value(key),
            // A map a reading gives is held as the map it is, which `keys()` and `get(key)` are
            // asked of; a map a field's own store gives is the list of its entries, below.
            (Ty::Map(_), Value::Map(_))
            | (Ty::Text | Ty::Date | Ty::Time | Ty::Identifier, Value::Text(_))
            | (Ty::RichText, Value::Rich(_))
            | (Ty::List(_) | Ty::Set(_) | Ty::Map(_) | Ty::Collection(_), Value::List(_))
            | (Ty::List(_) | Ty::Collection(_), Value::Coll(_))
            | (Ty::Option(_), Value::Opt(_))
            | (Ty::Enum(_) | Ty::Result(..), Value::Enum(_))
            | (Ty::Tuple(_) | Ty::Filter(_) | Ty::Sort(_), Value::Tuple(_))
            | (Ty::Action, Value::Action(_))
            // A library's parameter that asks for a JSON document takes the one a caller parsed
            // or built: `std.json_text(value, key)` is reached from another package, so the
            // document crosses the call boundary as the value it already is.
            | (Ty::Json, Value::Json(_))
            | (Ty::File, Value::File(_))
            | (Ty::Actor, Value::Actor(_))
            | (Ty::View, _) => true,
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!(
                "a {} does not fit a parameter of type {}",
                v.kind_name(),
                ty.display(app)
            ))
        }
    }

    /// Turn arguments into wasm values in the current frame. A record parameter takes the
    /// resource it names, or a record value of that type, which the code reads: an action's
    /// frame marks the value as handed to it, so a change to it fails there.
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
                // A scalar a view carries from an event is boxed until it reaches the action it
                // is passed to: the box is the place it lands, never a value of its own. Only a
                // parameter of a scalar type unboxes: an option of one is a value as it is.
                let scalar = || unboxed(v.clone());
                match ty.abi_repr() {
                    // `Duration` takes whole seconds (`Value::Int`) only, never a flag; `Int`
                    // and `Choice` keep `Value::as_int` (which reads a `Bool` as 0 or 1).
                    Some(Repr::I64) if ty == Ty::Duration => match scalar() {
                        Value::Int(i) => Ok(Wasm::I64(i)),
                        _ => Err(mismatch()),
                    },
                    Some(Repr::I64) => scalar().as_int().map(Wasm::I64).ok_or_else(mismatch),
                    Some(Repr::F64) => match scalar() {
                        Value::Float(f) => Ok(Wasm::F64(f)),
                        _ => Err(mismatch()),
                    },
                    Some(Repr::Bool) => scalar()
                        .as_int()
                        .map(|i| Wasm::I32(i32::from(i != 0)))
                        .ok_or_else(mismatch),
                    Some(Repr::Handle) => {
                        let held = self
                            .param_value(&ty, v, &pkg.meta.app)
                            .map_err(|e| format!("argument `{}`: {e}", p.name))?;
                        let handle = self.frames.borrow_mut().alloc(held.clone())?;
                        if into_action && matches!(held, Value::Rec(_)) {
                            self.frames.borrow_mut().hand(handle)?;
                        }
                        Ok(Wasm::I32(handle))
                    }
                    None => Err(format!(
                        "argument `{}`: a {} does not fit a parameter of type {}",
                        p.name,
                        v.kind_name(),
                        ty.display(&pkg.meta.app)
                    )),
                }
            })
            .collect()
    }

    fn unmarshal(&self, app: &str, result: Option<&str>, w: Option<Wasm>) -> Result<Value, String> {
        let Some(ty) = result else {
            return Ok(Value::Unit);
        };
        let ty = parse_ty(ty, app)?;
        match (ty.abi_repr(), w) {
            (None, _) => Ok(Value::Unit),
            (Some(Repr::I64), Some(Wasm::I64(i))) => Ok(Value::Int(i)),
            (Some(Repr::F64), Some(Wasm::F64(f))) => Ok(Value::Float(f)),
            (Some(Repr::Bool), Some(Wasm::I32(i))) => Ok(Value::Bool(i != 0)),
            (Some(Repr::Handle), Some(Wasm::I32(h))) => Ok(self.frames.borrow().get(h)?.clone()),
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
        self.decide_delivered(ctx, aref, false)
            .map(|(decision, _)| decision)
    }

    /// [`Self::decide`] of a call that `admitted` says the dispatch delivered, admitted for its
    /// invoker on their device, and whether it waits on that device ([`Authority::undecided`]).
    fn decide_delivered(
        &self,
        ctx: &ExecCtx,
        aref: &ActionRef,
        admitted: bool,
    ) -> Result<(Decision, bool), RuntimeError> {
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
            app: AppId::from(pkg.meta.app.as_str()),
            class: meta.class,
            own: aref.author.as_ref().is_none_or(|a| *a == pkg.hash),
            module: pkg.hash.clone(),
            target: target_of(&pkg.meta, aref.args.first()),
            admitted,
        };
        Ok((
            self.authority.decide(&ctx.invoker, &ctx.chain, &info),
            self.authority.undecided(&ctx.invoker, &ctx.chain, &info),
        ))
    }

    /// Whether a rule of the invoker's or the workspace's asks first before `aref` runs, apart
    /// from the code that led to it ([`crate::Authority::asks_first`]).
    fn rule_asks(&self, invoker: &Actor, aref: &ActionRef) -> Result<bool, RuntimeError> {
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
            app: AppId::from(pkg.meta.app.as_str()),
            class: meta.class,
            own: true,
            module: pkg.hash.clone(),
            target: target_of(&pkg.meta, aref.args.first()),
            admitted: false,
        };
        Ok(self.authority.asks_first(invoker, &info))
    }

    /// Whether `admission` answers the ask the authority made of `invoker`'s call `aref`: the
    /// dispatch's always. A person's press answers it unless the action confirms on that press
    /// ([`Self::press_confirms`]); a rule that asks them first waits for their yes too, unless
    /// they held Shift.
    fn press_answers(
        &self,
        admission: Admission,
        invoker: &Actor,
        aref: &ActionRef,
    ) -> Result<bool, RuntimeError> {
        Ok(match admission {
            Admission::Not => false,
            Admission::Answered | Admission::Approved => true,
            Admission::Pressed { shift } => {
                !self.press_confirms(aref, shift) && (shift || !self.rule_asks(invoker, aref)?)
            }
        })
    }

    /// Whether `aref`, a call the run of the person's own press makes, waits for their yes as
    /// it would pressed alone ([`Self::press_confirms`]). The press confirms once, at the
    /// outermost destructive call it reaches: one an earlier call of the run was confirmed for
    /// already does not ask again, and what a call they confirmed calls in turn runs on that
    /// yes. What always confirms asks each time.
    fn step_confirms(&self, journal: &Journal, aref: &ActionRef) -> bool {
        let Some(press) = journal.pressed else {
            return false;
        };
        match self.confirms_of(&aref.action) {
            Some(Confirms::Always) => true,
            Some(Confirms::UnlessShift) if !press.shift => !journal.entries.iter().any(|entry| {
                matches!(entry, JournalEntry::Invoked { action, .. }
                        if self.confirms_of(action).is_some_and(|c| c.on_press(false)))
            }),
            _ => false,
        }
    }

    /// What a person's press of `action` waits for, as its package declares it.
    fn confirms_of(&self, action: &str) -> Option<Confirms> {
        let pkg = self.registry.borrow().action_package(action)?;
        pkg.action(action).map(ActionMeta::confirms)
    }

    /// Whether `action` leaves the system for good, itself or through an own action it calls:
    /// what nothing brings back, and what a person's yes to another action never covers.
    fn leaves(&self, action: &str) -> bool {
        self.registry
            .borrow()
            .action_package(action)
            .and_then(|pkg| pkg.action(action).map(|meta| meta.leaves))
            .unwrap_or(false)
    }

    /// Whether the yes that carried `journal` answers the call of `action`: the ask was for an
    /// action that leaves, and `action` is one its own app's code calls, so their yes was for
    /// what it reaches. Another app's action is that app's to answer.
    fn yes_covers(&self, journal: &Journal, action: &str) -> bool {
        let top = if journal.invoked.is_empty() {
            journal.action.as_deref().unwrap_or_default()
        } else {
            journal.invoked.as_str()
        };
        if !self.leaves(top) {
            return false;
        }
        let registry = self.registry.borrow();
        let package_of = |action: &str| registry.action_package(action).map(|pkg| pkg.hash.clone());
        match (package_of(top), package_of(action)) {
            (Some(top), Some(call)) => top == call,
            _ => false,
        }
    }

    /// Whether a person's own press of `aref`, with Shift held when `shift`, waits for their yes
    /// whatever the policy decides: what leaves the system always does, and what its app marks
    /// destructive does unless they held Shift ([`ActionMeta::confirms`], which
    /// its contract and its catalogue offer publish). The policy decides whether they may; this
    /// is that they mean to. Only a person's press: an agent's or an automation's call is
    /// decided by its grants alone.
    ///
    /// This is the one place a press's confirmation is decided, for every app alike.
    fn press_confirms(&self, aref: &ActionRef, shift: bool) -> bool {
        self.confirms_of(&aref.action)
            .is_some_and(|confirms| confirms.on_press(shift))
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

    /// Make the resource `new` describes, under `id` or a fresh one, as one change that is
    /// nobody's: the resource's making. Its text fields start at the words their type declares
    /// (the store reads an unwritten text field as empty, never as its default), what its type's
    /// `initial action` writes is in that change, and so are the marks of its type's migrations,
    /// which upgrade what an earlier version wrote and have nothing to do on what this one makes.
    /// A type with none of these, or whose origin derives its content, is made as the store makes
    /// any resource.
    ///
    /// # Errors
    ///
    /// A store failure, or initial content that failed: then nothing is made.
    pub(crate) fn make(
        &self,
        new: &crate::store::NewResource,
        id: Option<&str>,
    ) -> Result<ResourceRef, RuntimeError> {
        let Some((pkg, initial, migrations)) = self.making(new) else {
            let mut store = self.store.borrow_mut();
            return Ok(match id {
                Some(id) => store.create_with_id(new, id)?,
                None => store.create(new)?,
            });
        };
        let id = id.map_or_else(|| self.store.borrow_mut().fresh_id("r"), str::to_owned);
        let txn = self.store.borrow_mut().begin()?;
        match self.make_in(&pkg, initial.as_ref(), &migrations, txn, new, &id) {
            Ok((made, note)) => {
                self.store.borrow_mut().commit(txn, &note)?;
                Ok(made)
            }
            Err(error) => {
                self.store.borrow_mut().rollback(txn);
                Err(error)
            }
        }
    }

    /// [`Self::make`] in the transaction `txn` a write already has open: a root nobody made
    /// yet is made by the first write to it, so the records the write puts under it stand in a
    /// resource the store holds, and what it starts with is in the same change.
    ///
    /// # Errors
    ///
    /// As [`Self::make`].
    pub(crate) fn make_in_change(
        &self,
        new: &crate::store::NewResource,
        id: &str,
        txn: Txn,
    ) -> Result<ResourceRef, RuntimeError> {
        match self.making(new) {
            Some((pkg, initial, migrations)) => self
                .make_in(&pkg, initial.as_ref(), &migrations, txn, new, id)
                .map(|(made, _)| made),
            None => self
                .store
                .borrow_mut()
                .create_in(txn, new, id)
                .map_err(RuntimeError::from),
        }
    }

    /// What [`Self::make`] writes beyond the store's own making of a resource: the package
    /// whose type it is, with the initial action and migration marks the making carries, when
    /// the type has any. `None` when the store's own making is all of it.
    fn making(
        &self,
        new: &crate::store::NewResource,
    ) -> Option<(Rc<Package>, Option<ActionMeta>, Vec<String>)> {
        let pkg = {
            let registry = self.registry.borrow();
            registry
                .type_owner(&new.ty)
                .and_then(|owner| registry.package(owner))
        };
        let initial = pkg
            .as_ref()
            .and_then(|pkg| pkg.meta.initial_of(&new.ty).cloned());
        let migrations: Vec<String> = pkg.as_ref().map_or_else(Vec::new, |pkg| {
            pkg.meta
                .migrations_of(&new.ty)
                .map(|migration| migration.id.clone())
                .collect()
        });
        let derived = pkg.as_ref().is_some_and(|pkg| {
            pkg.meta
                .type_named(&new.ty)
                .and_then(|ty| ty.origin.as_deref())
                .is_some_and(tilolang::origin::is_derived)
        });
        let holds = {
            let registry = self.registry.borrow();
            registry
                .type_info(&new.ty)
                .is_some_and(|(_, info)| crate::held::holds_any(&info))
        };
        pkg.filter(|_| {
            !derived
                && (initial.is_some()
                    || holds
                    || !migrations.is_empty()
                    || !new.text_defaults.is_empty())
        })
        .map(|pkg| (pkg, initial, migrations))
    }

    /// [`Self::make`]'s writes in the transaction `txn`, and the note its change commits under.
    fn make_in(
        &self,
        pkg: &Package,
        initial: Option<&ActionMeta>,
        migrations: &[String],
        txn: Txn,
        new: &crate::store::NewResource,
        id: &str,
    ) -> Result<(ResourceRef, ChangeNote), RuntimeError> {
        let made = self.store.borrow_mut().create_in(txn, new, id)?;
        for (field, text) in &new.text_defaults {
            let starts = Mutation::SetText {
                field: field.clone(),
                text: text.clone(),
            };
            self.store.borrow_mut().apply(txn, &made, &starts)?;
        }
        for migration in migrations {
            self.store
                .borrow_mut()
                .mark_migrated_in(txn, id, migration)?;
        }
        let mut touched = std::collections::BTreeSet::from([made.id_string()]);
        // The records its fields hold are made with it, before its initial content reads them.
        let ty = self
            .registry
            .borrow()
            .type_info(&new.ty)
            .map(|(_, info)| info);
        if let Some(ty) = ty.filter(|ty| crate::held::holds_any(ty)) {
            let held = self
                .make_held(&made, &ty, &|_| false, &|at, m| {
                    self.store
                        .borrow_mut()
                        .apply(txn, at, m)
                        .map_err(|e| e.to_string())
                })
                .map_err(RuntimeError::Package)?;
            touched.extend(held);
        }
        let mut at = None;
        if let Some(initial) = initial {
            // The package's own code writes it, in a frame of its own over the making's
            // transaction: what it inserts starts with its own initial content too.
            let app = Actor::App(AppId::from(pkg.meta.app.as_str()));
            let journal = Journal {
                action: Some(initial.id.clone()),
                args: vec![Value::resource(&made)],
                invoker: app.clone(),
                app: None,
                chain: Vec::new(),
                from: None,
                at: None,
                entries: Vec::new(),
                pending: None,
                finished: false,
                served: None,
                approved_by: None,
                asked: false,
                pressed: None,
                effects: 0,
                parent: None,
                invocation: String::new(),
                invoked: String::new(),
            };
            let state = ActionState {
                txn,
                action: initial.id.clone(),
                primary: Some(made.clone()),
                invoker: app.clone(),
                suggestions: Vec::new(),
                queued: Vec::new(),
                scenario: false,
                invocations: Vec::new(),
                note: Box::default(),
                touched: std::collections::BTreeSet::new(),
                run: Box::new(RunState {
                    run: format!("make:{id}"),
                    journal,
                    cursor: 0,
                    invoker: app,
                    clock: None,
                    journaled: false,
                }),
            };
            self.frames
                .borrow_mut()
                .push(pkg.hash.clone(), FrameKind::Action(state));
            let wrote = self.start_content(pkg, initial, &made);
            at = Some(self.change_time());
            if let Some(FrameKind::Action(state)) =
                self.frames.borrow_mut().pop().map(|frame| frame.kind)
            {
                touched.extend(state.touched);
            }
            wrote?;
        }
        let note = ChangeNote {
            action: crate::store::CREATE_ACTION.to_owned(),
            args: Json::String(id.to_owned()),
            module: pkg.hash.0.clone(),
            at: Some(at.unwrap_or_else(|| self.clock_now())),
            touched: touched.into_iter().collect(),
            count: 1,
            ..ChangeNote::default()
        };
        Ok((made, note))
    }

    /// Write what the new resource `at` starts with, its type's `initial action`, in the frame on
    /// top now: part of the change that makes it.
    pub(crate) fn start_content(
        &self,
        pkg: &Package,
        initial: &ActionMeta,
        at: &ResourceRef,
    ) -> Result<(), RuntimeError> {
        let outer = self.seeding.replace(Some((at.clone(), 0)));
        let wrote = self.call_in_frame(
            pkg,
            &format!("a_{}", initial.id),
            &initial.params,
            &[Value::resource(at)],
            None,
        );
        *self.seeding.borrow_mut() = outer;
        wrote.map(drop)
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
            // A package compiled against a library this runtime does not hold calls a function
            // of it: the id says nothing to the person reading the failure, so name the function
            // where the compiler here knows it.
            let known = tilolang::stdlib::meta()
                .fns
                .iter()
                .find(|f| f.id == id)
                .map(|f| f.name.as_str());
            return trap(match known {
                Some(name) => format!(
                    "`std.{name}` is not in the standard library loaded here: this app was built \
                     against another version of it"
                ),
                None => format!(
                    "no package loaded here declares the function `{id}`: the app that declares \
                     it is not loaded"
                ),
            });
        };
        if !pkg.code {
            return trap(format!(
                "the function `{id}` belongs to {}, which this device does not run",
                pkg.meta.app
            ));
        }
        let Some(func) = pkg.fn_(&id) else {
            return trap(format!(
                "the package {} here declares no function `{id}`, which this app calls: the two \
                 were built against different versions",
                pkg.meta.app
            ));
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
            invoker: Some(ctx.invoker.clone()),
            chain: ctx.chain.iter().map(|m| m.0.clone()).collect(),
            module: pkg.hash.0.clone(),
            approved_by: ctx.approved_by.clone(),
            from: ctx.from.clone(),
            at: ctx.at.clone(),
            gesture: ctx.gesture.clone(),
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
        self.run_as(ctx, aref, (false, None))
    }

    fn run_as(
        &self,
        ctx: &ExecCtx,
        aref: &ActionRef,
        (asked, pressed): (bool, Option<Press>),
    ) -> Result<ActionOutcome, RuntimeError> {
        let aref = &self.completed(aref)?;
        let run = self.run_id(ctx);
        let journal = Journal {
            asked,
            pressed,
            ..ctx.journal(aref, run.clone())
        };
        match self.drive_chain(&run, journal) {
            Ok(RunOutcome::Done(v)) => Ok(ActionOutcome::Done(v)),
            Ok(waits) => Ok(ActionOutcome::Waits(waits)),
            Err(e) if failed(&e) => Ok(ActionOutcome::Failed(e.to_string())),
            Err(e) => Err(e),
        }
    }

    /// One action applied to many items (`many`), as one change.
    ///
    /// Each reference names the same action for a different item. They are decided once — one
    /// action, by one person, from one piece of code — and run in one transaction, so they are
    /// one change: one undo takes them all back, and history says how many items it changed.
    /// Each is a call at its own app's action API when the action is another app's, which keeps its
    /// own record's changes apart.
    fn invoke_many(
        &self,
        identity: Option<&RunId>,
        (invoker, app): (&Actor, Option<&str>),
        aref: &ActionRef,
        from: Option<&ResourceRef>,
        gesture: Option<&str>,
        admission: Admission,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let admitted = admission != Admission::Not;
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
            app: app.map(str::to_owned),
            chain,
            approved_by: None,
            identity: identity.cloned(),
            from: from.cloned(),
            at: None,
            gesture: gesture.map(str::to_owned),
        };
        match self.decide(&ctx, first)? {
            Decision::Allow => {
                self.run_many(&ctx, &refs)?;
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            Decision::Deny => Ok(InvokeOutcome::Denied),
            // The choice is the person's own press, or a call the dispatch admitted for them:
            // that answers what the action asks first, as one action's press does, and the
            // many runs as the one change it is.
            Decision::Ask if admitted && self.press_answers(admission, invoker, first)? => {
                let ctx = ExecCtx {
                    approved_by: Some(invoker.clone()),
                    ..ctx
                };
                self.run_many(&ctx, &refs)?;
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            Decision::Ask => self.wait_on_person(aref),
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
            journal: ctx.journal(first, run.clone()),
            run,
            cursor: 0,
            invoker: ctx.invoker.clone(),
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
        let (queued, suggested, txn, note) = match frame.map(|f| f.kind) {
            Some(FrameKind::Action(a)) => {
                let mut note = *a.note;
                note.at = Some(at);
                note.touched = a.touched.into_iter().collect();
                note.readers = self.change_readers(&note.touched, note.invoker.as_ref());
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
                for (who, aref) in suggested {
                    self.suggest(&who, &aref)?;
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

    /// A direct invocation by an actor, or through an action reference some code wrote.
    ///
    /// # Errors
    ///
    /// As [`Core::run_action`]; a trap is an error here since nobody journals it.
    pub fn invoke_direct(
        &self,
        invoker: &Actor,
        aref: &ActionRef,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_in(invoker, aref, None)
    }

    /// [`Core::invoke_direct`] as one part of the person's act `gesture`: every change the action
    /// makes names it, so an undo takes it back with the change it was made with (a picture a
    /// paste wrote, and the app naming it).
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub fn invoke_direct_in(
        &self,
        invoker: &Actor,
        aref: &ActionRef,
        gesture: Option<&str>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_as_in(
            None,
            (invoker, None),
            aref,
            aref.from.as_ref(),
            gesture,
            Admission::Not,
        )
    }

    /// [`Core::invoke_direct`] as the run `identity` when every replica can name it the same
    /// way (a trigger, run for a record at an instant, on whichever device comes to run it), so
    /// what it queues and writes is held once; and made from the resource `from`, what the
    /// resources it creates were made from, recorded in the change it makes.
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub(crate) fn invoke_direct_as(
        &self,
        identity: Option<&RunId>,
        invoker: &Actor,
        aref: &ActionRef,
        from: Option<&ResourceRef>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_as_in(identity, (invoker, None), aref, from, None, Admission::Not)
    }

    /// [`Core::invoke_direct`] of a call the dispatch already admitted for `invoker`: it asked
    /// whoever had to say yes before it delivered the call, so the run does not ask again for the
    /// call itself.
    ///
    /// `admission` says how it was admitted. When the person approved it on the system's confirm
    /// ([`Admission::Approved`]), the run is the one they answered: what the app's own code goes
    /// on to call runs on that yes. When they made it themselves with a
    /// press the dispatch attested on the system's own surfaces ([`Admission::Pressed`]), it is
    /// their act, as a press in the app's window is. A call delivered with neither (an agent's,
    /// an automation's, another app's process, the person's with no press behind it) still asks
    /// for what it goes on to call. Code an author wrote around
    /// the call (a block, a view of someone's) still asks for what its author may not do: the
    /// admission decided the invoker, not that code.
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub(crate) fn invoke_admitted(
        &self,
        (invoker, app): (&Actor, Option<&str>),
        aref: &ActionRef,
        from: Option<&ResourceRef>,
        admission: Admission,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_as_in(None, (invoker, app), aref, from, None, admission)
    }

    /// [`Core::invoke_direct`] of the person `invoker`'s own press in their app's window, as one
    /// part of the act `gesture`: the person acting, so what the app's code goes on to call runs
    /// as theirs, and the press answers what that code asks when undo takes the action back. An
    /// action that leaves the system confirms first, Shift or not; a rule that asks them first
    /// ([`crate::Authority::asks_first`]) waits for their yes too, unless they held Shift
    /// (`shift`) and the action can be undone, where the press itself is the yes.
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub(crate) fn invoke_pressed(
        &self,
        invoker: &Actor,
        aref: &ActionRef,
        from: Option<&ResourceRef>,
        (gesture, shift): (Option<&str>, bool),
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.invoke_direct_as_in(
            None,
            (invoker, None),
            aref,
            from,
            gesture,
            Admission::Pressed { shift },
        )
    }

    /// [`Core::invoke_direct_as`] as one part of the person's act `gesture`.
    fn invoke_direct_as_in(
        &self,
        identity: Option<&RunId>,
        (invoker, app): (&Actor, Option<&str>),
        aref: &ActionRef,
        from: Option<&ResourceRef>,
        gesture: Option<&str>,
        admission: Admission,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let admitted = admission != Admission::Not;
        // One action over many items is one change: its references are the change's own.
        if aref.action == MANY_ACTION {
            return self.invoke_many(identity, (invoker, app), aref, from, gesture, admission);
        }
        // What the invoker left out is computed now, on what they see, and recorded.
        let aref = &self.completed(aref)?;
        // An action of an app this runtime does not run is a child invocation at that app's
        // own action API: the host sends it on with this view's author, and the API decides it
        // there (action-system.md, "One invocation path").
        if self.foreign_action(&aref.action) {
            return self.invoke_foreign(aref);
        }
        // A verb the resource's app publishes without a package loading here (a channel's
        // `send`) is that app's too: the same action API, the same attribution.
        if let Some(invocation) = self.published_invocation(aref) {
            return self.invoke_published(&invocation?);
        }
        let mut chain = Vec::new();
        push_chain(&mut chain, aref.author.as_ref());
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            app: app.map(str::to_owned),
            chain,
            approved_by: None,
            identity: identity.cloned(),
            from: from.cloned(),
            at: None,
            gesture: gesture.map(str::to_owned),
        };
        // What was admitted is the call as its own package applies it: code an author wrote
        // around it was not.
        let admitted = admitted && {
            let registry = self.registry.borrow();
            let own = registry
                .action_package(&aref.action)
                .map(|pkg| pkg.hash.clone());
            ctx.chain.iter().all(|m| own.as_ref() == Some(m))
        };
        let (decision, undecided) = self.decide_delivered(&ctx, aref, admitted)?;
        // The person's device has not said yet what they may do of it: their device decides it,
        // as it decides what reaches it from anywhere.
        if undecided {
            return self.decided_on_the_device(aref);
        }
        // A person's own press, attested where it was made, or their yes on the system's confirm
        // to the call as it was shown: what the app's code calls on the way runs under it, as an
        // answer they gave runs on. A call the dispatch delivered as the
        // person with neither was admitted for itself alone, so what its code goes on to call
        // that asks still asks.
        let person = admitted
            && matches!(admission, Admission::Pressed { .. } | Admission::Approved)
            && app.is_none()
            && invoker
                .member_id()
                .is_some_and(crate::ids::MemberId::is_person);
        let ran = |outcome: ActionOutcome| match outcome {
            ActionOutcome::Done(v) => Ok(InvokeOutcome::Done(v)),
            ActionOutcome::Waits(waits) => Ok(waits.into()),
            ActionOutcome::Failed(m) => Err(RuntimeError::Trap(m)),
        };
        // The person's own press: what it calls that a rule asks them first about still asks.
        let pressed = match admission {
            Admission::Pressed { shift } if person => Some(Press { shift }),
            _ => None,
        };
        match decision {
            // What confirms on a person's own press does so even where the policy allows it:
            // the policy says they may, the yes says they mean to.
            Decision::Allow
                if pressed.is_some_and(|press| self.press_confirms(aref, press.shift)) =>
            {
                self.wait_on_person(aref)
            }
            Decision::Allow => ran(self.run_as(&ctx, aref, (person, pressed))?),
            Decision::Deny => Ok(InvokeOutcome::Denied),
            // It was admitted before it got here, on whoever had to say yes, or the person
            // pressed it, which answers what its code asks but not a rule that asks them first:
            // the yes is for this call, and for what it calls on the way only when the person made
            // the call themselves.
            Decision::Ask if admitted && self.press_answers(admission, invoker, aref)? => {
                let ctx = ExecCtx {
                    approved_by: Some(invoker.clone()),
                    ..ctx
                };
                ran(self.run_as(&ctx, aref, (person, pressed))?)
            }
            Decision::Ask => self.wait_on_person(aref),
        }
    }

    /// `aref` as `invoker` suggests it (`suggest act(args)`): admitted at Ask, so where the
    /// authority would let it run, it asks the person first all the same, and waits as its asked
    /// record until they answer; what it denies stays denied. A runtime with no dispatch to keep
    /// the call (a scenario's world, one of its own) has nobody to ask, and nothing answers the
    /// suggestion there: it waits, naming no call.
    ///
    /// # Errors
    ///
    /// As [`Core::invoke_direct`].
    pub(crate) fn invoke_asking(
        &self,
        invoker: &Actor,
        aref: &ActionRef,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let aref = &self.completed(aref)?;
        let mut chain = Vec::new();
        push_chain(&mut chain, aref.author.as_ref());
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            app: None,
            chain,
            approved_by: None,
            identity: None,
            from: aref.from.clone(),
            at: None,
            gesture: None,
        };
        match self.decide(&ctx, aref)? {
            Decision::Deny => Ok(InvokeOutcome::Denied),
            _ if self.invoker.borrow().is_none() => Ok(InvokeOutcome::Asked(String::new())),
            Decision::Allow | Decision::Ask => self.wait_on_person(aref),
        }
    }

    /// `aref`, sent through the dispatch for the person's device to decide: it runs, waits on
    /// their yes, or is refused there. With no dispatch here, nothing decides it: it is refused.
    fn decided_on_the_device(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        if self.invoker.borrow().is_none() {
            return Ok(InvokeOutcome::Denied);
        }
        Ok(match self.sent(aref, false)? {
            Delegated::Waits(call) => InvokeOutcome::Asked(call),
            Delegated::Ran(entry) => match *entry {
                JournalEntry::Invoked { result, .. } => InvokeOutcome::Done(result),
                JournalEntry::Failed { reason, .. } => return Err(RuntimeError::Trap(reason)),
                _ => InvokeOutcome::Denied,
            },
        })
    }

    /// `aref`, which the authority asks a person about before it runs: sent through the dispatch
    /// asking them, which keeps it as its invocation record until they answer, and runs it then.
    fn wait_on_person(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        Ok(match self.sent(aref, true)? {
            Delegated::Waits(call) => InvokeOutcome::Asked(call),
            Delegated::Ran(entry) => match *entry {
                JournalEntry::Invoked { result, .. } => InvokeOutcome::Done(result),
                JournalEntry::Failed { reason, .. } => return Err(RuntimeError::Trap(reason)),
                _ => InvokeOutcome::Denied,
            },
        })
    }

    /// Whether `action` is an action of an app this runtime does not run: one the host invokes
    /// at that app's action API.
    fn foreign_action(&self, action: &str) -> bool {
        self.registry
            .borrow()
            .action_package(action)
            .is_some_and(|pkg| pkg.foreign)
    }

    /// The invocation a view's action of a *published* verb makes: the action `aref` names, on
    /// the resource its first argument names, when no loaded package declares it and the host
    /// publishes it on that resource ([`crate::published`]).
    ///
    /// A published verb is the resource's own, as a declared action is its package's: Chat's
    /// channel publishes `send`, and the app that publishes it runs it. This is one path for
    /// both languages — a Rust view naming `send` on the channel it shows, and a tilolang view
    /// naming it, make the same invocation with the same arguments by name.
    fn published_invocation(
        &self,
        aref: &ActionRef,
    ) -> Option<Result<crate::delegate::Invocation, RuntimeError>> {
        // A package with code runs its own actions; the host declares actions it does not run,
        // which are the resource's own verbs, as an app the session serves publishes them.
        let declared = self
            .registry
            .borrow()
            .action_package(&aref.action)
            .map(|pkg| pkg.meta.app.clone());
        if declared
            .as_deref()
            .is_some_and(|app| app != tilolang::HOST_APP)
        {
            return None;
        }
        let Value::Ref(target) = aref.args.first()? else {
            return None;
        };
        // A host action on an invocation record is the record's own: the app the workspace
        // publishes answers it (its invocations provider), whether or not this window
        // published the verb. What a person answers an ask with, and stops or runs a turn
        // again with, reaches the dispatch from the app whose view draws the ask.
        if declared.as_deref() == Some(tilolang::HOST_APP)
            && tilolang::is_invocation_type(&target.target.ty)
        {
            return Some(self.invocation_of(aref));
        }
        let id = target.target.id_string();
        let published = self.published.borrow();
        // A record addressed with the workspace it is in is published under its id there.
        let verb = published.get(&id, &aref.action).or_else(|| {
            target
                .target
                .item
                .as_deref()
                .and_then(|item| published.get(item, &aref.action))
        });
        let Some(verb) = verb else {
            // Declared by the host and published by no resource here: nothing in this host can
            // run it, and saying so is better than a module that is not there.
            return declared.is_some().then(|| {
                Err(RuntimeError::Package(format!(
                    "`{}` is answered by the app its workspace publishes, and this host publishes none",
                    aref.action
                )))
            });
        };
        // The verb's parameters as its app declares them, after the resource the address names
        // (`send(to: List, attached: List, text: Text)`); the resource is the target, never an argument.
        let args: Vec<(String, Value)> = verb
            .params
            .iter()
            .cloned()
            .zip(aref.args.iter().skip(1).cloned())
            .collect();
        Some(Ok(crate::delegate::Invocation::Action {
            app: AppId::from(verb.app.as_str()),
            action: aref.action.clone(),
            name: aref.action.clone(),
            args,
            class: crate::meta::EffectClass::Undoable,
            destructive: false,
            author: aref.author.clone(),
            target: Some(ResourceRef::clone(&target.target)),
            from: aref.from.clone(),
            asks: false,
            shows: Vec::new(),
        }))
    }

    /// Send the published invocation `invocation` on, as [`Core::invoke_foreign`] sends an
    /// action of a declared foreign app's.
    fn invoke_published(
        &self,
        invocation: &crate::delegate::Invocation,
    ) -> Result<InvokeOutcome, RuntimeError> {
        use crate::delegate::Invoked;
        let invoker = self.invoker.borrow();
        let Some(invoker) = invoker.as_ref() else {
            return Err(RuntimeError::Package(format!(
                "`{}` is published by `{}`, an app this host does not run, and it makes no \
                 invocation of it",
                invocation.name(),
                invocation.app()
            )));
        };
        Ok(match invoker.invoke(invocation) {
            Invoked::Done(result) => InvokeOutcome::Done(result),
            // Nothing waits on a call made outside a run: what it changes comes back as its
            // app's readings.
            Invoked::Pending(_) => InvokeOutcome::Done(Value::Unit),
            Invoked::Failed(reason) => return Err(RuntimeError::Trap(reason)),
            Invoked::Refused(_) => InvokeOutcome::Denied,
        })
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
            Invoked::Pending(_) => InvokeOutcome::Done(Value::Unit),
            Invoked::Failed(reason) => return Err(RuntimeError::Trap(reason)),
            // The action API said no: nothing ran here.
            Invoked::Refused(_) => InvokeOutcome::Denied,
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
        invoker: &Actor,
        action: &str,
        args: Vec<Value>,
        chain: Vec<ModuleHash>,
    ) -> Result<RunOutcome, RuntimeError> {
        let ctx = ExecCtx {
            invoker: invoker.clone(),
            app: None,
            chain,
            approved_by: None,
            identity: None,
            from: None,
            at: None,
            gesture: None,
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
        let journal = ctx.journal(aref, run.clone());
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
        if outermost {
            self.scenario_stopped.set(false);
        }
        let invocation = journal.invocation.clone();
        if outermost && !invocation.is_empty() {
            self.store.borrow_mut().begin_run(&invocation);
        }
        let mut outcome = match self.drive(run, journal) {
            Ok(RunEnd::Done { journal, value }) => self.answer_parent(&journal, Ok(value)),
            Ok(RunEnd::Waits(waits)) => Ok(waits),
            Ok(RunEnd::Failed { journal, error }) => self.answer_parent(&journal, Err(error)),
            Err(error) => Err(error),
        };
        if outermost {
            self.running.set(false);
            // A scenario stopped at a step it may not take keeps the steps before it, which were
            // changes of their own; it still answers why it stopped.
            let stopped = self.scenario_stopped.replace(false);
            // And it answers that it stopped, apart from a failure: its caller (a seed into a
            // person's workspace) says what it skipped rather than taking it for a broken seed.
            if stopped && let Err(error) = outcome {
                outcome = Err(RuntimeError::Stopped(match error {
                    RuntimeError::Trap(why) => why,
                    other => other.to_string(),
                }));
            }
            if !invocation.is_empty() {
                let mut store = self.store.borrow_mut();
                let settled = match &outcome {
                    // A run waiting on another app's answer is still one action: what it wrote
                    // stays apart until it ends, and a failure after the answer takes it all back.
                    Ok(RunOutcome::Waits { .. }) => Ok(()),
                    Ok(_) | Err(RuntimeError::Stopped(_)) => store.end_run(&invocation),
                    Err(_) => {
                        store.abort_run(&invocation);
                        Ok(())
                    }
                };
                drop(store);
                // A run whose writes the store refused is no success: the answer says why,
                // rather than standing for changes the workspace never took.
                let outcome = match settled {
                    Ok(()) => outcome,
                    Err(error) => Err(RuntimeError::Trap(format!(
                        "the store refused what the run wrote: {error}"
                    ))),
                };
                // A run that waits has not ended: it goes on as a new one, and the view waiting
                // on it keeps waiting.
                if !matches!(outcome, Ok(RunOutcome::Waits { .. })) {
                    self.ended(&invocation, outcome.is_ok());
                }
                return outcome;
            }
        }
        outcome
    }

    /// Say the invocation `invocation` ended, `done` or not, to a view waiting on it to run what
    /// follows it.
    fn ended(&self, invocation: &str, done: bool) {
        if let Some(end) = self.awaited.borrow_mut().get_mut(invocation) {
            *end = Some(done);
        }
    }

    /// [`Self::ended`] from the host that runs an invocation a view waits on to end, such as a
    /// pressed action a person answered: the call it got its yes for.
    pub(crate) fn call_ended(&self, invocation: &str, done: bool) {
        self.ended(invocation, done);
    }

    /// Wait on the invocation `call` to end, which a view knows by its id ([`Self::awaited_end`]
    /// says the end of once it ended).
    pub(crate) fn await_call(&self, call: &str) {
        self.awaited.borrow_mut().insert(call.to_owned(), None);
    }

    /// Wait on the invocation the run `run` is part of to end: the run at the top of its calls,
    /// which [`Self::awaited_end`] says the end of once it ended.
    pub(crate) fn await_end(&self, run: &RunId) -> Result<RunId, RuntimeError> {
        let invocation = self
            .store
            .borrow_mut()
            .load_journal(run)?
            .map(|journal| journal.invocation)
            .filter(|invocation| !invocation.is_empty())
            .unwrap_or_else(|| run.clone());
        self.awaited.borrow_mut().insert(invocation.clone(), None);
        Ok(invocation)
    }

    /// How the invocation a view waits on ended: `Some(true)` done, `Some(false)` failed or
    /// refused, `None` while it still runs. An ended one is no longer waited on.
    pub(crate) fn awaited_end(&self, invocation: &str) -> Option<bool> {
        let mut awaited = self.awaited.borrow_mut();
        let end = awaited.get(invocation).copied().flatten();
        if end.is_some() {
            awaited.remove(invocation);
        }
        end
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
            ModuleHash::written_by(&journal.invoker)
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
                clock: clock.clone(),
                journaled,
                journal,
            };
            let attempted = self.attempt(&pkg, &meta, &module, program, state);
            if !matches!(attempted, Ok(Attempted::Suspended { .. })) {
                self.forget_listings(run);
            }
            match attempted? {
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
                    self.freeze_listings(run, &mut journal);
                    match journal.pending.clone() {
                        Some(Pending::Call { call, .. }) => {
                            self.store.borrow_mut().save_journal(run, &journal)?;
                            let run = run.clone();
                            return Ok(RunEnd::Waits(RunOutcome::Waits { run, call }));
                        }
                        Some(Pending::Step(step)) => {
                            if let Some(waits) = self.take_step(run, &mut journal, &step)? {
                                // The step waits, on a person or on another app: a resume
                                // goes on from here.
                                self.store.borrow_mut().save_journal(run, &journal)?;
                                return Ok(RunEnd::Waits(waits));
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
            app: journal.app.clone(),
            chain: journal.chain.clone(),
            approved_by: journal.approved_by.clone(),
            identity: Some(run.clone()),
            from: journal.from.clone(),
            at: journal.at.clone(),
            gesture: None,
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
        if !state.journal.invoked.is_empty() {
            note.invoked = Some(state.journal.invoked.clone());
        }
        let kind = match txn {
            Some(txn) => FrameKind::Action(ActionState {
                txn,
                action: action.clone(),
                primary,
                invoker: ctx.invoker,
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
                note.readers = self.change_readers(&note.touched, note.invoker.as_ref());
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
                    suggested: a.suggestions,
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
                    // The action is not done, so this step keeps no migration or trigger mark:
                    // the change it ends with does.
                    if let Some(mut change) = change {
                        change.note.unfinished = true;
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
    /// blocks it settled and ask what it suggested.
    fn commit_segment(&self, invoker: &Actor, change: Change) -> Result<(), RuntimeError> {
        self.store.borrow_mut().commit(change.txn, &change.note)?;
        self.settle_blocks(invoker, change.queued)?;
        for (who, aref) in change.suggested {
            self.suggest(&who, &aref)?;
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
    ) -> Result<Option<RunOutcome>, RuntimeError> {
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
            // The app the run's own invoker calls through; a person a scenario names calls
            // through none.
            app: if step.who.is_some() {
                None
            } else {
                journal.app.clone()
            },
            chain,
            approved_by: None,
            identity: None,
            from: step.from.clone().or_else(|| journal.from.clone()),
            at: step.at.clone(),
            gesture: None,
        };
        // A verb the resource's app publishes without a package loading here: the same action API,
        // sent as a step of the run that asked for it.
        if let Some(invocation) = self.published_invocation(&aref) {
            // A verb the resource's app publishes is a call of the run like any other: the run
            // waits on the answer the dispatch gives it.
            match self.delegated_step(&aref.action, &invocation?)? {
                Delegated::Ran(entry) => {
                    journal.entries.push(*entry);
                    Ok(None)
                }
                Delegated::Waits(call) => {
                    let action = aref.action.clone();
                    journal.pending = Some(Pending::Call {
                        call: call.clone(),
                        action,
                    });
                    Ok(Some(RunOutcome::Waits {
                        run: run.clone(),
                        call,
                    }))
                }
            }
        // An action of an app this runtime does not run goes to that app; a host that holds
        // no records sends every invocation on to the app that does.
        } else if self.invoker.borrow().is_some()
            && (self.delegates_own.get() || self.foreign_action(&aref.action))
        {
            self.sent_step(run, journal, &aref, false)
        } else {
            let d = if step.seed {
                Decision::Allow
            } else {
                self.decide(&ctx, &aref)?
            };
            // A person allowed this run, or made it themselves, and what it reaches it reaches
            // through the calls its own code makes: an action that calls an outside one is
            // outside for that call, so it was asked about as a whole. The call that would ask
            // again is the one they allowed, unless code they were not asked about stands in its
            // chain.
            //
            // A run the person pressed answered what its code asks, not a rule that asks them
            // first: a call such a rule names waits for their yes, as it would pressed alone.
            let ruled = match journal.pressed {
                Some(press) if d == Decision::Ask => {
                    self.step_confirms(journal, &aref)
                        || (!press.shift && self.rule_asks(&ctx.invoker, &aref)?)
                }
                _ => false,
            };
            let allowed = journal
                .approved_by
                .clone()
                .or_else(|| journal.asked.then(|| journal.invoker.clone()))
                .filter(|_| {
                    d == Decision::Ask
                        && !ruled
                        && self.asked_about(journal, &ctx.chain, &aref.action)
                });
            // A call that confirms, made in a run the person pressed, waits for their yes even
            // where the policy allows it, as it would pressed alone.
            let confirms = !step.seed && self.step_confirms(journal, &aref);
            // What leaves the system asks in a run the person's yes carried for something else:
            // their yes for filing a job is no yes to a letter leaving, and their yes for one
            // action that leaves answers what its own app's code reaches, not another app's.
            let outside = !step.seed
                && journal.asked
                && self.leaves(&aref.action)
                && !self.yes_covers(journal, &aref.action);
            match d {
                Decision::Ask if allowed.is_some() && !outside => {
                    let ctx = ExecCtx {
                        approved_by: allowed,
                        ..ctx
                    };
                    self.run_step(run, journal, &ctx, &aref)
                }
                Decision::Allow if confirms || outside => self.sent_step(run, journal, &aref, true),
                Decision::Allow => self.run_step(run, journal, &ctx, &aref),
                Decision::Deny => {
                    journal.entries.push(JournalEntry::Denied {
                        action: step.action.clone(),
                        reason: "denied by authority".to_owned(),
                    });
                    Ok(None)
                }
                Decision::Ask => self.sent_step(run, journal, &aref, true),
            }
        }
    }

    /// Whether the call of `action` by the code in `chain` is one the person who allowed
    /// `journal`'s run was asked about: code of the chain the run was asked about, and, in the
    /// run they answered or pressed themselves, the package of the action they allowed, calling
    /// an action of that package. Another app's action it calls still asks: they acted in this
    /// app, not that one. A run started afresh holds no answer of theirs for its calls.
    fn asked_about(&self, journal: &Journal, chain: &[ModuleHash], action: &str) -> bool {
        let registry = self.registry.borrow();
        let package_of = |action: &str| registry.action_package(action).map(|pkg| pkg.hash.clone());
        let own = journal
            .action
            .as_deref()
            .filter(|_| journal.asked)
            .and_then(&package_of);
        if own.is_some() && package_of(action) != own {
            return false;
        }
        chain
            .iter()
            .all(|m| journal.chain.contains(m) || own.as_ref() == Some(m))
    }

    /// Send `aref` through the dispatch as a step of `run` (asking a person first when `asks`),
    /// and journal what it came to: `Some` when the run waits on its answer.
    fn sent_step(
        &self,
        run: &RunId,
        journal: &mut Journal,
        aref: &ActionRef,
        asks: bool,
    ) -> Result<Option<RunOutcome>, RuntimeError> {
        match self.sent(aref, asks)? {
            Delegated::Ran(entry) => {
                journal.entries.push(*entry);
                Ok(None)
            }
            Delegated::Waits(call) => {
                let action = aref.action.clone();
                journal.pending = Some(Pending::Call {
                    call: call.clone(),
                    action,
                });
                let run = run.clone();
                Ok(Some(RunOutcome::Waits { run, call }))
            }
        }
    }

    /// Send `aref` through the dispatch, asking a person first when `asks`. A host with no
    /// dispatch makes no call that needs a yes: it is refused.
    fn sent(&self, aref: &ActionRef, asks: bool) -> Result<Delegated, RuntimeError> {
        if asks && self.invoker.borrow().is_none() {
            return Ok(Delegated::Ran(Box::new(JournalEntry::Denied {
                action: aref.action.clone(),
                reason: "nobody here can be asked".to_owned(),
            })));
        }
        let mut invocation = self.invocation_of(aref)?;
        if let crate::delegate::Invocation::Action {
            asks: asking,
            shows,
            ..
        } = &mut invocation
        {
            *asking = asks;
            if asks {
                *shows = self.asked_shows(aref)?;
            }
        }
        self.delegated_step(&aref.action, &invocation)
    }

    /// What a call of `aref` shows the person asked to allow it: each entry of its action's
    /// `shows:`, in its order, labelled as the field (or the parameter) is in the person's
    /// language, with the value it holds now. A field is read through this run, so what the run
    /// wrote before the call (the draft an answer made) is what the person reads. A secret's
    /// value never leaves the run: the person reads that there is one, not what it is.
    ///
    /// An entry that calls a `fn` (`invites(text)`) shows what the function answers for the
    /// call's arguments, read through this run as a field is; a list it answers reads one item
    /// to a line.
    ///
    /// What a question can show is bounded, and it never hides that it is: its lists of
    /// addresses name every one of them up to [`MOST_SHOWN_ADDRESSES`] together, and more is
    /// refused with how many they name rather than silently cut, as is an address the person
    /// could misread ([`reads_plainly`]); a long value is shown by its start, with a line saying
    /// how many characters follow it. A cut stops nothing from leaving: the call itself is what
    /// its person approves, so the person must read it whole or not at all.
    fn asked_shows(&self, aref: &ActionRef) -> Result<Vec<crate::delegate::Shown>, RuntimeError> {
        let Some((params, listed)) = self
            .registry
            .borrow()
            .action_package(&aref.action)
            .and_then(|pkg| {
                pkg.action(&aref.action)
                    .map(|meta| (meta.params.clone(), meta.shows.clone()))
            })
        else {
            return Ok(Vec::new());
        };
        let mut shown = Vec::with_capacity(listed.len());
        let mut addresses = 0_usize;
        for entry in listed {
            let given = params
                .iter()
                .position(|p| p.name == entry.param)
                .and_then(|at| aref.args.get(at));
            let (label, value) = match (&entry.call, &entry.field) {
                (Some(call), _) => self.asked_call(aref, &params, &entry.param, call)?,
                (None, None) => (tilolang::messages::label(&entry.param), given.cloned()),
                (None, Some(name)) => self.asked_field(given, name)?,
            };
            let mut value = match value {
                // A secret's value never leaves the run.
                _ if entry.render.as_deref() == Some("secret") => String::new(),
                None => String::new(),
                Some(value) => self.shown_text(value),
            };
            let mut continues = None;
            match entry.render.as_deref() {
                // A list of addresses is shown whole: its person allows what they read, so one
                // too long to read, or one they could misread, is no question to ask at all.
                Some("addresses") => {
                    addresses = addresses.saturating_add(addresses_in(&value));
                    if addresses > MOST_SHOWN_ADDRESSES {
                        return Err(RuntimeError::Trap(format!(
                            "{addresses} addresses are more than the {MOST_SHOWN_ADDRESSES} one \
                             question can show"
                        )));
                    }
                    if let Some(misread) = named_addresses(&value).find(|a| !reads_plainly(a)) {
                        let start: String =
                            misread.chars().take(MOST_SHOWN_ADDRESS_CHARS).collect();
                        return Err(RuntimeError::Trap(format!(
                            "the address {start:?} cannot be shown so that it reads for what it \
                             is: it holds letters outside plain ASCII or a quoted part, or is \
                             longer than {MOST_SHOWN_ADDRESS_CHARS} characters"
                        )));
                    }
                }
                Some("secret") => {}
                // A long value is shown by its start, and the question says what it leaves out.
                render => {
                    let most = if render == Some("long") {
                        MOST_SHOWN_LONG_CHARS
                    } else {
                        MOST_SHOWN_TEXT_CHARS
                    };
                    let text = value.trim();
                    let whole = text.chars().count();
                    let shown = if whole > most {
                        continues = Some(format!(
                            "{} more characters are not shown here",
                            whole.saturating_sub(most)
                        ));
                        text.chars().take(most).collect()
                    } else {
                        text.to_owned()
                    };
                    value = shown;
                }
            }
            shown.push(crate::delegate::Shown {
                label: label.clone(),
                value,
                render: entry.render.clone(),
            });
            if let Some(continues) = continues {
                shown.push(crate::delegate::Shown {
                    label: format!("{label} continues"),
                    value: continues,
                    render: None,
                });
            }
        }
        Ok(shown)
    }

    /// `value` as the person asked reads it: text as it is, a list one item to a line, anything
    /// else as it projects.
    fn shown_text(&self, value: Value) -> String {
        match crate::host_res::unboxed(value) {
            Value::Text(text) => text,
            Value::List(items) => items
                .into_iter()
                .map(|item| self.shown_text(item))
                .collect::<Vec<_>>()
                .join("\n"),
            other => {
                crate::project::project(self, &other, crate::project::ProjectOptions::default())
                    .to_string()
            }
        }
    }

    /// What the `fn` `call` names answers for the arguments `aref` gives the parameters it names,
    /// as this run reads it, labelled as the function `name` is in the person's language.
    fn asked_call(
        &self,
        aref: &ActionRef,
        params: &[tilolang::meta::ParamMeta],
        name: &str,
        call: &tilolang::meta::ShownCall,
    ) -> Result<(String, Option<Value>), RuntimeError> {
        let Some(pkg) = self.registry.borrow().fn_package(&call.function) else {
            return Err(RuntimeError::Trap(format!(
                "what the ask shows calls `{name}`, which no package loaded here declares"
            )));
        };
        let Some(func) = pkg.fn_(&call.function).cloned() else {
            return Err(RuntimeError::Trap(format!(
                "what the ask shows calls `{name}`, which {} does not declare",
                pkg.meta.app
            )));
        };
        let args: Vec<Value> = call
            .args
            .iter()
            .map(|arg| {
                params
                    .iter()
                    .position(|p| p.name == *arg)
                    .and_then(|at| aref.args.get(at))
                    .cloned()
                    .unwrap_or(Value::Missing)
            })
            .collect();
        let value = self.call_framed(
            &pkg,
            FrameKind::Call,
            &format!("n_{}", call.function),
            &func.params,
            &args,
            Some(&func.result),
        )?;
        let fallback = tilolang::messages::label(name);
        let label =
            self.words
                .borrow()
                .word(&tilolang::messages::key_of(&call.function), &[], &fallback);
        Ok((label, Some(value)))
    }

    /// The field `name` of the record `arg` refers to, as this run reads it, with its label in
    /// the person's language; no value when the argument names no record of that type.
    fn asked_field(
        &self,
        arg: Option<&Value>,
        name: &str,
    ) -> Result<(String, Option<Value>), RuntimeError> {
        let fallback = tilolang::messages::label(name);
        let Some(Value::Ref(at)) = arg else {
            return Ok((fallback, None));
        };
        let target = &at.target;
        let field = {
            let registry = self.registry.borrow();
            registry
                .type_id(&target.ty)
                .and_then(|ty| registry.type_info(&ty))
                .and_then(|(_, info)| info.fields.iter().find(|f| f.name == name).cloned())
        };
        let Some(field) = field else {
            return Ok((fallback, None));
        };
        let value = self
            .read_value(target, &field, None)
            .map_err(RuntimeError::Trap)?;
        let label =
            self.words
                .borrow()
                .word(&tilolang::messages::key_of(&field.id), &[], &fallback);
        Ok((label, Some(value)))
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
    ) -> Result<Option<RunOutcome>, RuntimeError> {
        // The call is a step of the run: the run's effects before it and its calls after it
        // are the same on a device that runs the invocation again from the top, so what the
        // call does is held under the keys the first device used.
        let child = format!("{run}/{}", journal.entries.len());
        // The call's changes say which action was invoked: the one at the top of the calls.
        let invoked = if journal.invoked.is_empty() {
            journal.action.clone().unwrap_or_default()
        } else {
            journal.invoked.clone()
        };
        let child_journal = Journal {
            parent: Some(run.clone()),
            asked: journal.asked,
            pressed: journal.pressed,
            invoked,
            ..ctx.journal(aref, journal.invocation.clone())
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
            RunEnd::Waits(waits) => {
                self.store.borrow_mut().save_journal(run, journal)?;
                Ok(Some(waits))
            }
        }
    }

    /// Answer the call the run `run` made of another app's action and waits on with `answer`,
    /// and run on from it: the code gets the result, or the failure as an error value.
    ///
    /// A refusal is the invocation as a whole not happening — the person whose yes it waited on
    /// said no — so nothing of the run stands: what it wrote is taken back out, and the run ends
    /// refused.
    ///
    /// # Errors
    ///
    /// `NotPending` when the run waits on no call; `Denied` when the answer refuses the run;
    /// otherwise as running it on.
    pub fn answer_call(
        &self,
        run: &RunId,
        answer: crate::delegate::Invoked,
    ) -> Result<RunOutcome, RuntimeError> {
        use crate::delegate::Invoked;
        let mut journal = self.waiting_journal(run)?;
        let Some(Pending::Call { action, .. }) = journal.pending.take() else {
            return Err(RuntimeError::NotPending(run.clone()));
        };
        let entry = match answer {
            Invoked::Done(result) => JournalEntry::Invoked { action, result },
            Invoked::Failed(reason) => JournalEntry::Failed { action, reason },
            Invoked::Pending(_) => return Err(RuntimeError::NotPending(run.clone())),
            // A refusal is the invocation as a whole not happening: the run ends refused, and
            // what it wrote is taken back out.
            Invoked::Refused(reason) => {
                journal.finished = true;
                journal.entries.push(JournalEntry::Denied {
                    action,
                    reason: reason.clone(),
                });
                self.store.borrow_mut().save_journal(run, &journal)?;
                if !journal.invocation.is_empty() {
                    self.store.borrow_mut().abort_run(&journal.invocation);
                    self.ended(&journal.invocation, false);
                }
                return Err(RuntimeError::Denied(reason));
            }
        };
        journal.entries.push(entry);
        self.store.borrow_mut().save_journal(run, &journal)?;
        self.drive_chain(run, journal)
    }

    /// End the run `run`, which waits on a call nobody can answer any more: what it wrote is
    /// taken back, and the reason it ended is its last journaled step.
    ///
    /// A host that restarts over a store still holding a waiting run has no invocation to answer
    /// the call with, and no record of what the run held, so the run is ended here rather than
    /// waiting for ever.
    ///
    /// # Errors
    ///
    /// `NotPending` when the run waits on nothing; `NotFound` when no run is; a store failure.
    pub fn abandon_run(
        &self,
        run: &RunId,
        reason: &str,
    ) -> Result<Option<(u64, String)>, RuntimeError> {
        let mut journal = self.waiting_journal(run)?;
        let served = journal.served.clone();
        let action = match journal.pending.take() {
            Some(Pending::Call { action, .. }) => action,
            _ => journal.action.clone().unwrap_or_default(),
        };
        journal.finished = true;
        journal.entries.push(JournalEntry::Denied {
            action,
            reason: reason.to_owned(),
        });
        self.store.borrow_mut().save_journal(run, &journal)?;
        if !journal.invocation.is_empty() {
            self.store.borrow_mut().abort_run(&journal.invocation);
            self.ended(&journal.invocation, false);
        }
        Ok(served)
    }

    /// Record that the run `run` is the invocation `request` (its own id `invocation`) the host
    /// serves: what a host starting over the store answers when the run cannot go on.
    ///
    /// # Errors
    ///
    /// `NotFound` when no run is; a store failure.
    pub fn serving(&self, run: &RunId, request: u64, invocation: &str) -> Result<(), RuntimeError> {
        let mut journal = self
            .store
            .borrow_mut()
            .load_journal(run)?
            .ok_or_else(|| RuntimeError::NotFound(format!("run {run}")))?;
        journal.served = Some((request, invocation.to_owned()));
        self.store.borrow_mut().save_journal(run, &journal)?;
        Ok(())
    }

    /// The journal of `run`, which waits on something.
    fn waiting_journal(&self, run: &RunId) -> Result<Journal, RuntimeError> {
        let journal = self
            .store
            .borrow_mut()
            .load_journal(run)?
            .ok_or_else(|| RuntimeError::NotFound(format!("run {run}")))?;
        if journal.finished || journal.pending.is_none() {
            return Err(RuntimeError::NotPending(run.clone()));
        }
        Ok(journal)
    }

    /// Run a read of a resource. A run that may invoke another action records the value in its
    /// journal, and running it again answers from the journal; anywhere else the read just runs.
    pub(crate) fn journaled_read(&self, live: impl FnOnce() -> HResult<Value>) -> HResult<Value> {
        if self.writing.get() {
            // A read a write makes on its way is the write's own: the journal answers the write.
            return live();
        }
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

    /// Make one write the running code asks for (`r.f = v`, an insert, a removal, a move) as one
    /// step of its run's journal. A run that is run again after a call it waited on made this
    /// write before the call, and the change it made is committed under the run's invocation:
    /// the journal answers the whole write, so the replay neither checks it against the store,
    /// which already holds it (the item it removed is gone), nor decides from the store what to
    /// write (where a move puts an item), and it makes the write no more. The run goes on from
    /// where it waited, on the state it was in then. `made` names the item the write made, which
    /// `replayed` builds the write's answer from.
    pub(crate) fn journaled_write<T>(
        &self,
        live: impl FnOnce() -> HResult<T>,
        made: impl FnOnce(&T) -> Option<String>,
        replayed: impl FnOnce(Option<String>) -> HResult<T>,
    ) -> HResult<T> {
        if self.writing.get() {
            return live();
        }
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut().filter(|run| run.journaled) else {
                drop(frames);
                return live();
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Wrote { item } = entry else {
                    return trap("the run diverged from its journal: expected a write");
                };
                let item = item.clone();
                run.cursor = run.cursor.saturating_add(1);
                drop(frames);
                return replayed(item);
            }
        }
        self.writing.set(true);
        let result = live();
        self.writing.set(false);
        let value = result?;
        self.journal_push(JournalEntry::Wrote { item: made(&value) })?;
        Ok(value)
    }

    /// Apply a write the running action makes, as [`Core::journaled_read`] answers a read: a run
    /// that is run again after it waited made this write before the call it waited on, and the
    /// change it made is in the record. Replay answers the write from the journal and applies
    /// nothing, so the item it made keeps what the call wrote on top of it. Inside a
    /// [`Core::journaled_write`], the write is that one's own and is applied as it comes.
    pub(crate) fn journaled_apply(
        &self,
        txn: Txn,
        at: &ResourceRef,
        m: &Mutation,
    ) -> HResult<Applied> {
        if self.writing.get() {
            return self.apply_write(txn, at, m);
        }
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

    /// `made_from`: what the invocation is made from, the invocation a scenario pushed, else the
    /// run's.
    fn made_from(&self) -> HResult<Option<Wasm>> {
        let frames = self.frames.borrow();
        let FrameKind::Action(state) = &frames.top()?.kind else {
            return trap("made_from is only available in actions");
        };
        let from = state
            .invocations
            .last()
            .and_then(|inv| inv.from.clone())
            .or_else(|| state.note.from.clone());
        drop(frames);
        self.ret_h(Value::Opt(from.map(|target| {
            Box::new(Value::Ref(crate::value::RefValue {
                target: Rc::new(target),
                author: None,
            }))
        })))
    }

    /// `invoke(aref)` and `try act(args)`: the action runs as a change of its own at its own
    /// action API. The journal answers a call the run already made; otherwise the run stops here, and
    /// the call runs and the code goes on from its result.
    fn invoke(&self, h: i32) -> HResult<Option<Wasm>> {
        let Value::Action(aref) = self.val(h)? else {
            return trap("expected an action reference");
        };
        // What the call runs under: the invocation a scenario pushed for it, else the run's own.
        // The invocation names who runs it; the moment it acts at and the resource its changes
        // are made from are the invocation's, else the run's. A scenario's code is the host's own
        // making, so what it calls is seed data too: it runs without asking.
        let (from, who, at, seed) = {
            let frames = self.frames.borrow();
            match frames.top().map(|f| &f.kind) {
                Ok(FrameKind::Action(state)) => {
                    let invocation = state.invocations.last();
                    (
                        // A call made from a resource (`ask(a).made_from(m)`) is made from it.
                        aref.from
                            .clone()
                            .or_else(|| invocation.and_then(|inv| inv.from.clone()))
                            .or_else(|| state.note.from.clone()),
                        invocation.map(|inv| inv.who.clone()),
                        invocation
                            .and_then(|inv| inv.at.clone())
                            .or_else(|| state.note.at.clone()),
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
        // What did not run stops the action or the program that does not `try` it, with why: it
        // never goes on as if it had, holding the refusal where the call's answer belongs (a
        // comment the discussion refused to make is no comment to mark the words with).
        if let Value::Error(reason) = &value
            && !self.trying.get()
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
            app: AppId::from(pkg.meta.app.as_str()),
            action: aref.action.clone(),
            // What the app's action API takes it by: a compiled package's action by its name, an
            // action of a description of an app's surface (no module) by the verb it describes,
            // without the app and the resource type its id names
            // (`so.tilo.vision#ImageModel/cancel` is published as `cancel`), and one the host
            // declares by its id.
            name: if !pkg.meta.module_hash.is_empty() {
                meta.name.clone()
            } else if meta.verb.is_empty() {
                meta.id.clone()
            } else {
                meta.verb.clone()
            },
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
            target: meta
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
            asks: false,
            shows: Vec::new(),
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
            Invoked::Pending(call) => Delegated::Waits(call),
            Invoked::Failed(reason) => Delegated::Ran(Box::new(JournalEntry::Failed {
                action: action.to_owned(),
                reason,
            })),
            Invoked::Refused(reason) => Delegated::Ran(Box::new(JournalEntry::Denied {
                action: action.to_owned(),
                reason,
            })),
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
            (AppId::from(pkg.meta.app.as_str()), field.clone())
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
            // A program sets a field of the resource it names; a record's own fields change
            // through the resource's actions.
            path: Vec::new(),
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
            // A field edit is the app's own: what it changes comes back as its reading.
            Invoked::Done(_) | Invoked::Pending(_) => (
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

    /// `call(resource, action, arguments)` and `try_call(...)`: one action at the address the
    /// registry names, called on the device's dispatch, its answer kept for a replayed run.
    fn call_at(
        &self,
        imp: Imp,
        resource_h: i32,
        action_h: i32,
        arguments_h: i32,
    ) -> HResult<Option<Wasm>> {
        let (resource, action) = (self.text(resource_h)?, self.text(action_h)?);
        let arguments =
            crate::value::to_json(&self.val_whole(arguments_h)?).map_err(HostFail::Trap)?;
        let address = format!("{resource}#{action}");
        let answer = self.journaled_read(|| match self.invoker.borrow().as_ref() {
            Some(invoker) => match invoker.call(&address, &arguments) {
                Ok(value) => Ok(Value::Json(Box::new(value))),
                Err(reason) => Ok(Value::Error(reason)),
            },
            None => Ok(Value::Error(
                "This host calls no action by address".to_owned(),
            )),
        })?;
        if imp == Imp::TryCall {
            return match answer {
                Value::Error(reason) => self.result(Err(reason)),
                value => self.result(Ok(value)),
            };
        }
        if let Value::Error(reason) = answer {
            return trap(reason);
        }
        self.ret_h(answer)
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
        let (class, app) = self.registry.borrow().action_package(&action).map_or(
            (None, String::new()),
            |package| {
                (
                    package.action(&action).map(|a| a.class),
                    package.meta.app.clone(),
                )
            },
        );
        if class != Some(EffectClass::Outside) {
            return trap(format!("action `{action}` is not classed outside"));
        }
        let Some(primary) = primary else {
            return trap("an outside action needs a resource argument to queue its effect on");
        };
        self.journaled_apply(
            txn,
            &primary,
            &Mutation::EffectPush {
                app,
                capability,
                payload,
                held_until,
                key,
            },
        )?;
        self.ret_h(Value::Unit)
    }

    /// `withdraw(r)`: take back the outside effects `r`'s actions queued that nothing carried
    /// out yet, so they never happen. Only an action asks for it, of its own target, or the
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
            invoker: Some(invoker),
            ..ChangeNote::default()
        };
        self.store
            .borrow_mut()
            .withdraw_effects(&primary.target.doc, &reason, &note)
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
                            p.default.is_some()
                                || crate::platform::form_asks(&pkg.meta, &p.ty)
                                || crate::platform::chooser_fills(&pkg.meta, &p.ty)
                        })
                        .count();
                    (a.params.len(), defaulted, a.name.clone())
                })
            });
        match arity {
            // The system verbs every resource has are no package's actions: `card.share_link()`
            // takes the resource and nothing else, and the host answers it
            // ([`crate::host_ui::system_verb`]).
            None if crate::host_ui::system_verb(&action).is_some() => {
                if args.len() != 1 {
                    return trap(format!(
                        "the system action `{action}` takes its resource alone"
                    ));
                }
            }
            None => return trap(format!("unknown action `{action}`")),
            Some((n, defaulted, name))
                if args.len() > n || args.len() < n.saturating_sub(defaulted) =>
            {
                return trap(format!(
                    "action `{name}` ({action}) takes {n} arguments, got {}",
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

    pub(crate) fn actor_arg(&self, h: i32) -> HResult<Actor> {
        self.with_val(h, |v| match v {
            Value::Actor(p) => Ok(p.clone()),
            other => trap(format!("expected an actor, got {}", other.kind_name())),
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
                    note.invoker = Some(inv.who.clone());
                    note.at = Some(inv.at.clone().unwrap_or(at));
                    note.from = inv.from.clone().or_else(|| state.note.from.clone());
                }
                // At the moment the invocation named, when it named one; else now.
                None => note.at = note.at.clone().or(Some(at)),
            }
            note.touched = std::mem::take(&mut state.touched).into_iter().collect();
            note.readers = self.change_readers(&note.touched, note.invoker.as_ref());
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

    /// `suggest act(args)`: the call asks a person even where it may run without
    /// ([`Core::suggest`]). An action asks once it has committed, so the call is never part of
    /// its transaction; a program holds no transaction of its own, so it asks now.
    fn suggest_in_frame(&self, act: i32) -> HResult<Option<Wasm>> {
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
            self.suggest(&who, &aref)
                .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        }
        ret_void()
    }

    /// Whether the running scenario may run `action` as `who`. A runtime deciding by a real
    /// workspace's access (a seed run into a person's workspace) acts only as whoever runs the
    /// scenario: recording a change as anyone else, a member or not, would be doing it in their
    /// name. The refusal names the step, who it was to run as, and why; whoever ran the
    /// scenario names the scenario.
    fn may_act_as(&self, who: &Actor, action: &str) -> HResult<()> {
        if !self.members_decided.get() {
            return Ok(());
        }
        let runs_as = {
            let frames = self.frames.borrow();
            let FrameKind::Action(state) = &frames.top()?.kind else {
                return trap("only a scenario names people and acts as them");
            };
            state.invoker.clone()
        };
        if *who == runs_as {
            return Ok(());
        }
        // The steps before this one stand: the run that ends on this refusal keeps them.
        self.scenario_stopped.set(true);
        let name = self.named(who);
        if self.member(who).is_none() {
            trap(format!(
                "can't {action} as {name}: {name} is not in this workspace"
            ))
        } else {
            trap(format!(
                "can't {action} as {name}: {name} is not you, so this can't be done as them"
            ))
        }
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

    /// `window_state<T>(name, value)`: sets the declared window state `name` in the window the
    /// running action was invoked from, when the value is what its view declares there.
    fn window_state(&self, name_h: i32, ty_h: i32, value_h: i32) -> HResult<Option<Wasm>> {
        let name = self.text(name_h)?;
        let ty = self.text(ty_h)?;
        let value = self.val_whole(value_h)?;
        let Some(context) = self.invoking_window.borrow().clone() else {
            return trap(
                "`window_state` changes the state of the window an action was invoked from; this run has none",
            );
        };
        let (state_id, declared) = {
            let registry = self.registry.borrow();
            let Some(pkg) = registry.package(&context.module) else {
                return trap("`window_state` no longer knows the window's view");
            };
            let Some(view) = pkg.view(&context.view) else {
                return trap(format!(
                    "`window_state` no longer knows the view `{}`",
                    context.view
                ));
            };
            let Some(state) = view.state.iter().find(|state| state.name == name) else {
                return trap(format!("the window's view declares no state `{name}`"));
            };
            (state.id.clone(), state.ty.clone())
        };
        if declared != ty {
            return trap(format!(
                "`window_state` writes `{name}` as {declared}, not {ty}"
            ));
        }
        let who = self.looking()?;
        let key = crate::view_state::StateKey::view(context.window, who, state_id);
        self.view_states.borrow_mut().write(key, value);
        Ok(None)
    }

    /// `notice(text, kind, undo)`: what the app says in passing in the window the running action
    /// was invoked from. The window's bar shows it for a few seconds, with the person's undo as
    /// its one button when `undo` says so; a newer one replaces it, and saying nothing
    /// (`text` empty) takes it down. Nothing is written to the workspace and no device is told:
    /// a run nothing looks through says it to nobody. The run's journal answers a run that
    /// replays after an effect it waited on, so a notice said before that call is said once.
    fn notice_import(&self, text_h: i32, kind_h: i32, undo_h: i32) -> HResult<Option<Wasm>> {
        let text = self.text(text_h)?;
        let kind = crate::notice::Kind::parse(&self.text(kind_h)?);
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                self.said.borrow_mut().push(crate::notice::Said {
                    text,
                    kind,
                    undo: undo_h != 0,
                });
                return ret_void();
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                if !matches!(entry, crate::run::JournalEntry::Said) {
                    return trap("the run diverged from its journal: expected a notice's entry");
                }
                run.cursor = run.cursor.saturating_add(1);
                return ret_void();
            }
        }
        self.said.borrow_mut().push(crate::notice::Said {
            text,
            kind,
            undo: undo_h != 0,
        });
        self.journal_push(crate::run::JournalEntry::Said)?;
        ret_void()
    }

    /// `notify(to, title, body, opens)`: tell the person `to` something, once. The notice is a
    /// record written in this action's own change ([`crate::notify`]), so every device of theirs
    /// reads it. The run's journal answers a run that replays, so it tells them no more than the
    /// first run did.
    fn notify_import(
        &self,
        to: i32,
        title: i32,
        body: i32,
        opens: i32,
        device: i32,
        actions: i32,
    ) -> HResult<Option<Wasm>> {
        let to = self.actor_arg(to)?;
        let title = self.text(title)?;
        let body = self.text(body)?;
        let device = self.text(device)?;
        // The actions it offers, by name, as the list they were given.
        let actions: Vec<String> = match self.val(actions)? {
            Value::List(items) => {
                let mut names = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        Value::Text(name) => names.push(name.clone()),
                        other => {
                            return trap(format!(
                                "notify: `on:` names actions, and this is a {}",
                                other.kind_name()
                            ));
                        }
                    }
                }
                names
            }
            Value::Missing => Vec::new(),
            other => {
                return trap(format!(
                    "notify: `on:` is a list of action names, and this is a {}",
                    other.kind_name()
                ));
            }
        };
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
            device,
            actions,
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
        // A rehearsal keeps what it would tell, and tells no one: its run is taken back.
        if let Some(rehearsed) = self.rehearsed.borrow_mut().as_mut() {
            rehearsed.push((id, notice));
        } else if let Some(host) = self.notices.borrow().as_ref() {
            host.raised(&notice);
        }
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
        use tilolang::{
            NOTICE_ACTIONS, NOTICE_BODY, NOTICE_DEVICE, NOTICE_OPENS, NOTICE_TITLE, NOTICE_TO,
            NOTICE_TYPE,
        };

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
                    value: scalar(&Value::Actor(notice.to.clone()), &crate::meta::Ty::Actor)?,
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
            (
                NOTICE_DEVICE.to_owned(),
                crate::store::Mutation::SetText {
                    field: NOTICE_DEVICE.to_owned(),
                    text: notice.device.clone(),
                },
            ),
            (
                NOTICE_ACTIONS.to_owned(),
                crate::store::Mutation::SetRegister {
                    field: NOTICE_ACTIONS.to_owned(),
                    // A list is kept as any value of the language is (`{"list": [...]}`), so the
                    // read that offers the notice's actions reads it back as one.
                    value: crate::store::Scalar::Json(
                        crate::value::to_json(&Value::List(
                            notice
                                .actions
                                .iter()
                                .map(|name| Value::Text(name.clone()))
                                .collect(),
                        ))
                        .map_err(|error| HostFail::Trap(format!("notify: actions: {error}")))?,
                    ),
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
            Imp::Call | Imp::TryCall => self.call_at(imp, a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::Suggest => self.suggest_in_frame(a.h(0)?),
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
            Imp::Notify => self.notify_import(a.h(0)?, a.h(1)?, a.h(2)?, a.h(3)?, a.h(4)?, a.h(5)?),
            Imp::Notice => self.notice_import(a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::WindowState => self.window_state(a.h(0)?, a.h(1)?, a.h(2)?),
            // Who invokes the action running, or the person a view is shown to: who would
            // invoke its actions. An automation's run is the automation: whom it writes for is
            // `invoker.principal()`.
            Imp::Invoker => {
                let who = self.looking()?;
                self.ret_h(Value::Actor(who))
            }
            Imp::MadeFrom => self.made_from(),
            Imp::PersonNamed | Imp::AgentNamed => {
                let name = self.text(a.h(0)?)?;
                self.scenario_state(|_| ())?;
                let who = if imp == Imp::AgentNamed {
                    self.named_agent(&name)?
                } else {
                    self.named_person(&name)?
                };
                self.ret_h(Value::Actor(who))
            }
            Imp::InvocationPush => {
                let who = self.actor_arg(a.h(0)?)?;
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
                let action = self.text(a.h(3)?)?;
                // What the scenario did before this step is a change of its own already, so a
                // step refused here stops it with that kept.
                self.scenario_segment()?;
                self.may_act_as(&who, &action)?;
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
            Imp::ActorEq => {
                let (x, y) = (self.actor_arg(a.h(0)?)?, self.actor_arg(a.h(1)?)?);
                ret_i32(i32::from(x == y))
            }
            Imp::ActorName => {
                let p = self.actor_arg(a.h(0)?)?;
                self.ret_h(Value::Text(self.named(&p)))
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
        // A read of the clock, a draw, a fetch and a page's answer answer the code that made
        // them where they are answered; a write is answered where it is made, and a run that
        // waited answers it there. None of them is a step's value.
        JournalEntry::Clock(_)
        | JournalEntry::Random { .. }
        | JournalEntry::Web { .. }
        | JournalEntry::Wrote { .. }
        | JournalEntry::Notified
        | JournalEntry::Said
        | JournalEntry::Engine { .. }
        | JournalEntry::Listing { .. }
        | JournalEntry::Page { .. } => Value::Unit,
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

/// The resource `first`, the action's first argument, names when the action addresses one: its
/// published id and the kind of its type, read from `meta`.
fn target_of(
    meta: &tilolang::meta::PackageMeta,
    first: Option<&crate::value::Value>,
) -> Option<crate::authority::ActionTarget> {
    let crate::value::Value::Ref(reference) = first? else {
        return None;
    };
    let ty = meta
        .types
        .iter()
        .find(|declared| declared.id == reference.target.ty)?;
    Some(crate::authority::ActionTarget {
        document: format!("tl:{}:{}", meta.app, reference.target.doc),
        item: reference.target.item.clone(),
        kind: format!("tl.{}.{}", meta.app, ty.name),
    })
}

/// How a direct call reached the runtime, which says whether what the workspace asks first was
/// answered before it got here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Nobody admitted it: decided here as the invoker's call.
    Not,
    /// The dispatch admitted it, having asked whoever had to say yes.
    Answered,
    /// The dispatch admitted it again on the person's yes on the system's confirm, to the call as
    /// it was shown to them.
    Approved,
    /// The person pressed it in their app's window, with Shift held or not.
    Pressed { shift: bool },
}
