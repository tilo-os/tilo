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

/// Where outside effects are delivered once settled.
pub trait OutsideSink {
    /// # Errors
    ///
    /// A message when the capability cannot be delivered; the entry stays queued.
    fn deliver(&mut self, capability: &str, payload: &Json) -> Result<(), String>;
}

pub struct Core {
    me: Weak<Core>,
    pub engine: Engine,
    pub registry: RefCell<Registry>,
    pub frames: RefCell<Frames>,
    pub store: RefCell<Box<dyn Store>>,
    pub authority: Box<dyn Authority>,
    pub models: RefCell<Option<Box<dyn ModelHost>>>,
    pub sink: RefCell<Option<Box<dyn OutsideSink>>>,
}

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
        Ok(Rc::new_cyclic(|me| Self {
            me: me.clone(),
            engine,
            registry: RefCell::new(Registry::default()),
            frames: RefCell::new(Frames::default()),
            store: RefCell::new(store),
            authority,
            models: RefCell::new(None),
            sink: RefCell::new(None),
        }))
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
    fn dispatch(&self, imp: Imp, mem: &[u8], a: &Args<'_>) -> HResult<Option<Wasm>> {
        use Imp::{
            Abort, ActRef, CollInsert, CollMove, CollRemove, FloatToStr, IntToStr, Invoke, IsError,
            IsSome, ListGetF, ListGetH, ListGetI, ListLen, ListNew, ListPushF, ListPushH,
            ListPushI, ListSortByKey, MapGetH, MapKeys, MapPut, MapRemove, ModelCall, NoneV,
            Outside, RecNew, RecSetF, RecSetH, RecSetI, RefEq, RefId, RefLit, ResAddF, ResAddI,
            ResGetF, ResGetH, ResGetI, ResSetF, ResSetH, ResSetI, ResTextSet, SetAdd, SetRemove,
            SomeF, SomeH, SomeI, SpaceF, SpaceI, StrCmp, StrConcat, StrEq, StrLen, StrLit,
            TupleGetF, TupleGetH, TupleGetI, TupleNew, TupleSetF, TupleSetH, TupleSetI, UiAnchor,
            UiBind, UiChild, UiNode, UiOn, UiPropF, UiPropH, UiPropI, UnwrapF, UnwrapH, UnwrapI,
        };
        match imp {
            StrLit | StrConcat | StrEq | StrLen | IntToStr | FloatToStr | StrCmp => {
                self.text_import(imp, mem, a)
            }
            ListNew | ListPushI | ListPushF | ListPushH | ListLen | ListGetI | ListGetF
            | ListSortByKey | ListGetH | TupleNew | TupleSetI | TupleSetF | TupleSetH
            | TupleGetI | TupleGetF | TupleGetH | SomeH | SomeI | SomeF | NoneV | IsSome
            | UnwrapI | UnwrapF | UnwrapH => self.aggregate_import(imp, a),
            ResGetI | ResGetF | ResGetH | ResSetI | ResSetF | ResSetH | ResAddI | ResAddF
            | ResTextSet | CollInsert | CollRemove | CollMove | SetAdd | SetRemove | MapPut
            | MapRemove | MapGetH | MapKeys | RecNew | RecSetI | RecSetF | RecSetH | RefLit
            | RefEq | RefId => self.resource_import(imp, a),
            ActRef | Invoke | IsError | Outside | ModelCall => self.effect_import(imp, a),
            UiNode | UiPropH | UiPropI | UiPropF | UiChild | UiBind | UiOn | UiAnchor | SpaceI
            | SpaceF => self.ui_import(imp, a),
            Abort => {
                let msg = self.text(a.h(0)?)?;
                trap(format!("abort: {msg}"))
            }
        }
    }
}
