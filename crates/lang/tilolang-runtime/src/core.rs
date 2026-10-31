//! The host core: the state every import and every invocation shares.
//!
//! `Core` implements [`Host`], so the engine calls straight into it. Everything is behind
//! `RefCell`s and no borrow is held across a call into guest code, so a flow's `invoke` can
//! run an action (which pushes a frame, runs the guest and pops it) while the flow's own
//! frame is live.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::{Rc, Weak};

use serde_json::Value as Json;

use crate::abi::{self, Imp};
use crate::authority::Authority;
use crate::engine::{Engine, EngineLimits, Host, HostFail, Wasm};
use crate::handles::Frames;
use crate::registry::Registry;
use crate::store::Store;
use crate::value::Value;

pub type HResult<T> = Result<T, HostFail>;

pub fn trap<T>(msg: impl Into<String>) -> HResult<T> {
    Err(HostFail::Trap(msg.into()))
}

impl From<String> for HostFail {
    fn from(msg: String) -> Self {
        Self::Trap(msg)
    }
}

/// Answers `model_call`. Calls are journaled, so an implementation may be nondeterministic.
pub trait ModelHost {
    /// # Errors
    ///
    /// A message when the model cannot answer.
    fn call(&self, kind: &str, input: &Value) -> Result<Value, String>;
}

/// What became of an outside effect handed to a sink (convergence.md, "What people see").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The service took it: it is done.
    Delivered,
    /// The service refused it, for this reason. It is never tried again.
    Rejected(String),
    /// Handed over; the service has not answered yet. Asked again on the next settle.
    ///
    /// Resolved by the sink that says it: it is obliged to have its owner settle again once
    /// the service answered (an origin reports `Polled::settle` from the poll that took the
    /// answer in), and to answer every effect it took, its service going away included.
    Pending,
    /// The service could not be reached, for this reason, so it never got it: the effect waits
    /// to go online and is tried again when the network comes back.
    Offline(String),
}

/// Where outside effects are delivered once settled.
pub trait OutsideSink {
    /// Hand the effect to the service, or say what became of it since it was handed over.
    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery;
}

/// How a principal is called where a view shows it (`principal.name()`).
pub trait NameHost {
    /// The name to show for `who`.
    fn name(&self, who: &crate::ids::Principal) -> String;
}

pub struct Core {
    me: Weak<Core>,
    pub engine: Engine,
    pub registry: RefCell<Registry>,
    pub frames: RefCell<Frames>,
    pub store: RefCell<Box<dyn Store>>,
    pub authority: Box<dyn Authority>,
    pub models: RefCell<Option<Box<dyn ModelHost>>>,
    /// What names principals; the id itself until the host says.
    pub names: RefCell<Option<Box<dyn NameHost>>>,
    /// The workspace's members, as the host last gave them (`workspace.members`).
    pub members: RefCell<crate::members::Members>,
    pub sink: RefCell<Option<Box<dyn OutsideSink>>>,
    /// The network came back: entries waiting to go online are tried again on the next settle.
    pub retry_waiting: std::cell::Cell<bool>,
    /// The platform's phrases in the person's language (`phrase(...)`); the id itself until
    /// the host says.
    pub phrases: RefCell<Option<crate::platform::Phrases>>,
    /// Compiled embedded blocks.
    pub embeds: RefCell<crate::embed::EmbedState>,
    /// The `window` and `lens` state of every view.
    pub view_states: RefCell<crate::view_state::ViewStates>,
    /// What `now` reads: the current time as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`. Read
    /// once per outermost invocation; until the host sets one, it is the epoch.
    pub clock: RefCell<Option<Box<dyn Fn() -> String>>>,
    /// How many seconds the person's clock is ahead of UTC, which `dated` reads a day and a
    /// time typed against; UTC until the host says.
    pub zone: RefCell<Option<Box<dyn Fn() -> i64>>>,
    /// References to collection items built so far: what a view materialized of the
    /// collections it read.
    pub items_built: std::cell::Cell<u64>,
    /// What the program being run printed and kept, while one runs.
    pub program: RefCell<Option<crate::program::ProgramIo>>,
    /// What finds resources by text for programs' `find`.
    pub text_search: RefCell<Option<Box<crate::program::TextSearch>>>,
    /// Where a task's programs send what they change, in a host that runs no app's code
    /// ([`crate::delegate`]); `None` where the runtime runs actions itself.
    pub invoker: RefCell<Option<Box<dyn crate::delegate::Invoker>>>,
    /// Where a task's programs ask each app what they read of it (`grep`, `find`, `read`,
    /// `describe`), while they run: a host that holds no records; `None` where the runtime
    /// answers what it holds itself.
    pub reader: RefCell<Option<Box<crate::program::ReadThrough>>>,
    /// Whether the invocation being made is a `try`'s, whose failure the program handles.
    pub trying: std::cell::Cell<bool>,
    /// The resources of apps this runtime does not run: how they print, and their readings, as
    /// the host gives them.
    pub foreign: RefCell<crate::foreign::Foreign>,
}

/// What `now` reads while the host has set no clock.
pub const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Typed access to the wasm arguments of one import call.
pub struct Args<'a>(&'a [Wasm]);

impl Args<'_> {
    pub fn h(&self, i: usize) -> HResult<i32> {
        match self.0.get(i) {
            Some(Wasm::I32(v)) => Ok(*v),
            _ => trap(format!("argument {i} is not an i32")),
        }
    }

    pub fn long(&self, i: usize) -> HResult<i64> {
        match self.0.get(i) {
            Some(Wasm::I64(v)) => Ok(*v),
            _ => trap(format!("argument {i} is not an i64")),
        }
    }

    pub fn float(&self, i: usize) -> HResult<f64> {
        match self.0.get(i) {
            Some(Wasm::F64(v)) => Ok(*v),
            _ => trap(format!("argument {i} is not an f64")),
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_i32(v: i32) -> HResult<Option<Wasm>> {
    Ok(Some(Wasm::I32(v)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_i64(v: i64) -> HResult<Option<Wasm>> {
    Ok(Some(Wasm::I64(v)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_f64(v: f64) -> HResult<Option<Wasm>> {
    Ok(Some(Wasm::F64(v)))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every import returns through the same result shape"
)]
pub fn ret_void() -> HResult<Option<Wasm>> {
    Ok(None)
}

pub fn ordering_to_i32(o: Ordering) -> i32 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

impl Core {
    /// # Errors
    ///
    /// A message when the engine cannot be built.
    pub fn new(
        store: Box<dyn Store>,
        authority: Box<dyn Authority>,
        limits: EngineLimits,
    ) -> Result<Rc<Self>, String> {
        let engine = Engine::new(limits)?;
        let core = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            engine,
            registry: RefCell::new(Registry::default()),
            frames: RefCell::new(Frames::default()),
            store: RefCell::new(store),
            authority,
            models: RefCell::new(None),
            names: RefCell::new(None),
            members: RefCell::new(crate::members::Members::default()),
            sink: RefCell::new(None),
            retry_waiting: std::cell::Cell::new(false),
            phrases: RefCell::new(None),
            embeds: RefCell::new(crate::embed::EmbedState::default()),
            view_states: RefCell::new(crate::view_state::ViewStates::default()),
            clock: RefCell::new(None),
            zone: RefCell::new(None),
            items_built: std::cell::Cell::new(0),
            program: RefCell::new(None),
            text_search: RefCell::new(None),
            invoker: RefCell::new(None),
            reader: RefCell::new(None),
            trying: std::cell::Cell::new(false),
            foreign: RefCell::new(crate::foreign::Foreign::default()),
        });
        // The types the host provides to every module, such as `Invocation`.
        let host = crate::registry::Package::new(
            crate::ids::ModuleHash("host".to_owned()),
            tilolang::host_meta(),
        )?;
        core.registry.borrow_mut().add(host)?;
        Ok(core)
    }

    /// A strong handle to this core, for passing to the engine as the host.
    pub fn rc(&self) -> Result<Rc<Self>, String> {
        self.me
            .upgrade()
            .ok_or_else(|| "the runtime has been dropped".to_owned())
    }

    /// Put a value in the current frame and answer its handle.
    pub fn ret_h(&self, v: Value) -> HResult<Option<Wasm>> {
        let h = self.frames.borrow_mut().alloc(v)?;
        ret_i32(h)
    }

    /// A copy of the value behind a handle.
    pub fn val(&self, h: i32) -> HResult<Value> {
        Ok(self.frames.borrow().get(h)?.clone())
    }

    /// Run `f` on the value behind a handle without copying it.
    pub fn with_val<R>(&self, h: i32, f: impl FnOnce(&Value) -> HResult<R>) -> HResult<R> {
        let frames = self.frames.borrow();
        f(frames.get(h)?)
    }

    pub fn text(&self, h: i32) -> HResult<String> {
        self.with_val(h, |v| match v {
            Value::Text(s) => Ok(s.clone()),
            other => trap(format!("expected text, got {}", other.kind_name())),
        })
    }

    pub fn module_of_frame(&self) -> HResult<crate::ids::ModuleHash> {
        Ok(self.frames.borrow().top()?.module.clone())
    }
}

impl Host for Core {
    fn import(&self, id: usize, memory: &[u8], args: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        let Some(imp) = abi::imp(id) else {
            return trap(format!("unknown import {id}"));
        };
        self.dispatch(imp, memory, &Args(args))
            .map_err(|e| match e {
                HostFail::Trap(m) => HostFail::Trap(format!("{}: {m}", abi::name(id))),
                suspend @ HostFail::Suspend => suspend,
            })
    }
}

impl Core {
    /// The host clock's time now, or the epoch without one.
    pub(crate) fn clock_now(&self) -> String {
        self.clock
            .borrow()
            .as_ref()
            .map_or_else(|| EPOCH.to_owned(), |clock| clock())
    }
}

impl Core {
    fn dispatch(&self, imp: Imp, mem: &[u8], a: &Args<'_>) -> HResult<Option<Wasm>> {
        use Imp::{
            Abort, ActOpen, ActRef, ActSet, ActThen, Actions, ArgAt, CollInsert, CollMove,
            CollMoveAmong, CollMoveTo, CollOrdered, CollRemove, CollWhere, CollWhereWith, Dated,
            Delivery, EnumGetF, EnumGetH, EnumGetI, EnumIs, EnumNew, EnumSetF, EnumSetH, EnumSetI,
            EventPart, EventValue, Filters, FloatToStr, IdentPart, IntToStr, Invoke, Invoker,
            InvokerPop, InvokerPush, IsError, IsSome, Lenses, ListEnd, ListGetF, ListGetH,
            ListGetI, ListLen, ListNew, ListPushF, ListPushH, ListPushI, ListReverse,
            ListSortByKey, MapGetH, MapKeys, MapPut, MapRemove, MapSetAdd, MapSetRemove, Members,
            MetaOf, ModelCall, NoneV, Now, Outside, Phrase, PrincipalEq, PrincipalKind,
            PrincipalName, PrincipalNamed, PrincipalRole, Query, QueueBlock, RecNew, RecSetF,
            RecSetH, RecSetI, RefEq, RefId, RefIs, RefLit, ResAddF, ResAddI, ResGetF, ResGetH,
            ResGetI, ResSetF, ResSetH, ResSetI, ResTextSet, RichEmpty, RichNew, RichPart, RichTime,
            SetAdd, SetRemove, SomeF, SomeH, SomeI, SpaceF, SpaceI, Speakers, StateAt, StateGet,
            StrCmp, StrConcat, StrEq, StrLen, StrLit, TryInvoke, TupleGetF, TupleGetH, TupleGetI,
            TupleNew, TupleSetF, TupleSetH, TupleSetI, UiActions, UiAnchor, UiBind, UiBindState,
            UiChild, UiNode, UiOn, UiPropF, UiPropH, UiPropI, UnwrapF, UnwrapH, UnwrapI, ValueEq,
            VersionEnd, Versions, WorkspaceMembers,
        };
        match imp {
            StrLit | StrConcat | StrEq | StrLen | IntToStr | FloatToStr | StrCmp => {
                self.text_import(imp, mem, a)
            }
            ListNew | ListPushI | ListPushF | ListPushH | ListLen | ListGetI | ListGetF
            | ListSortByKey | ListReverse | ListGetH | ListEnd | TupleNew | TupleSetI
            | TupleSetF | TupleSetH | TupleGetI | TupleGetF | TupleGetH | SomeH | SomeI | SomeF
            | NoneV | IsSome | UnwrapI | UnwrapF | UnwrapH => self.aggregate_import(imp, a),
            CollWhere => {
                let found = self.coll_where(a.h(0)?, a.h(1)?)?;
                self.ret_h(found)
            }
            CollWhereWith => {
                let found = self.coll_where_with(a.h(0)?, a.h(1)?, a.h(2)?)?;
                self.ret_h(found)
            }
            CollOrdered => {
                let found = self.coll_ordered(a.h(0)?, a.h(1)?)?;
                self.ret_h(found)
            }
            Query => {
                let found = self.query_import(a.h(0)?, a.h(1)?, a.h(2)?, a.h(3)?)?;
                self.ret_h(found)
            }
            Filters => {
                let filters = self.filters_import(a.h(0)?)?;
                self.ret_h(filters)
            }
            IdentPart => {
                let part = self.ident_part(a.h(0)?, a.h(1)?)?;
                self.ret_h(part)
            }
            RichNew | RichPart | RichEmpty | RichTime => self.rich_import(imp, a),
            EnumNew | EnumSetI | EnumSetF | EnumSetH | EnumIs | EnumGetI | EnumGetF | EnumGetH
            | ValueEq => self.enum_import(imp, a),
            ResGetI | ResGetF | ResGetH | ResSetI | ResSetF | ResSetH | ResAddI | ResAddF
            | ResTextSet | CollInsert | CollRemove | CollMove | CollMoveTo | CollMoveAmong
            | SetAdd | SetRemove | MapPut | MapRemove | MapSetAdd | MapSetRemove | MapGetH
            | MapKeys | RecNew | RecSetI | RecSetF | RecSetH | RefLit | RefEq | RefIs | RefId => {
                self.resource_import(imp, a)
            }
            ActRef | Invoke | TryInvoke | IsError | Outside | ModelCall | Invoker | PrincipalEq
            | PrincipalName | QueueBlock | Now | PrincipalNamed | InvokerPush | InvokerPop => {
                self.effect_import(imp, a)
            }
            UiNode | UiPropH | UiPropI | UiPropF | UiChild | UiBind | UiOn | UiAnchor | SpaceI
            | SpaceF | UiBindState | StateGet | StateAt | ArgAt | ActSet | ActThen | ActOpen
            | UiActions => self.ui_import(imp, a),
            EventValue | EventPart | Actions | Lenses | Delivery | MetaOf | Versions
            | VersionEnd | Phrase | Dated => self.platform_import(imp, a),
            WorkspaceMembers | Members | Speakers | PrincipalKind | PrincipalRole => {
                self.members_import(imp, a)
            }
            Imp::EditField => self.flow_edit(a),
            Imp::Print
            | Imp::Find
            | Imp::ReadText
            | Imp::OfType
            | Imp::Keep
            | Imp::Grep
            | Imp::Describe
            | Imp::DescribeRef
            | Imp::ReadWith
            | Imp::PrintRows
            | Imp::CountBy
            | Imp::GroupBy
            | Imp::Take
            | Imp::Single
            | Imp::Me
            | Imp::ForeignRef
            | Imp::MatchGroup => self.program_import(imp, a),
            Abort => {
                let msg = self.text(a.h(0)?)?;
                trap(format!("abort: {msg}"))
            }
        }
    }
}
