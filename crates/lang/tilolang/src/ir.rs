//! The typed IR between the checker and the wasm backend. Structured control flow, explicit
//! host calls, no names.

use crate::abi::Imp;
use crate::types::{EffectClass, Ty};
use std::collections::BTreeSet;

/// Index of a local (parameters first) in a function.
pub type LocalId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    BoolToInt,
    IntToBool,
    IntToFloat,
    FloatToInt,
    /// Sign-extends a host `i32` result (such as `str_cmp`) to an Int.
    SignExt,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: Ek,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub enum Ek {
    Int(i64),
    Float(f64),
    /// `10s`: a length of time, as its seconds. Only where a length of time is a value (the
    /// moment an outside effect is held until); a filter's is resolved by the query instead.
    Duration(i64),
    Bool(bool),
    /// A raw `i32` constant (an index or a count in the host ABI).
    I32(i32),
    /// A string literal as a handle (`str_lit` of a data segment).
    Str(String),
    Local(LocalId),
    /// A host import call. The expression's type says whether the result is kept.
    Host(Imp, Vec<Expr>),
    /// A call of a function of this module, by index into [`Module::funcs`].
    Call(usize, Vec<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Seq(Vec<Stmt>, Box<Expr>),
    Conv(Conv, Box<Expr>),
    /// No value: the end of a block without a final expression.
    Unit,
    /// A value never produced: the end of a block that returns on every path.
    Unreachable,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Set(LocalId, Expr),
    Eval(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    ForList(Box<ForList>),
    /// Leaves the innermost `ForList`.
    Break,
    Return(Option<Expr>),
}

/// `for item in list { body }` over a host list handle.
#[derive(Clone, Debug)]
pub struct ForList {
    pub list: Expr,
    pub list_local: LocalId,
    pub len_local: LocalId,
    pub idx_local: LocalId,
    pub item_local: LocalId,
    pub get: Imp,
    pub get_conv: Option<Conv>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncKind {
    Fn,
    Action,
    View,
    Block,
    /// An action's requirements its subject alone decides: it answers the first reason the
    /// action cannot run on the subject now, or the empty text while it can
    /// ([`guard_export`]).
    Guard,
    /// An action's `applies` conditions its subject and place decide: it answers the first
    /// reason the action cannot apply to the item where it is offered, or the empty text while
    /// it can ([`applies_export`]).
    Applies,
}

/// An action that code can apply: `app` and `id` name it for the host.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionRef {
    pub app: String,
    pub id: String,
    pub name: String,
    pub own: bool,
    /// Index of the function when the target is declared in this module.
    pub func: Option<usize>,
    /// The class recorded in the metadata of the foreign package.
    pub foreign_class: Option<EffectClass>,
}

/// What a function does, for the effect and capability analysis.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub mutates: bool,
    pub outside: bool,
    /// The capabilities the body's `outside(...)` calls name (`youtube.search`, `smtp.send`), as
    /// written: what its action is tagged with so the session can find the action that asks a
    /// service for what a canvas query names ([`crate::meta::ActionMeta::outside`]).
    pub outside_caps: BTreeSet<String>,
    pub invokes: BTreeSet<ActionRef>,
    pub action_values: BTreeSet<ActionRef>,
    pub views: BTreeSet<usize>,
    /// The effects the body's own code reads; what it reaches through a call is
    /// [`crate::effects::reached`]'s.
    pub effects: BTreeSet<crate::effects::Effect>,
    /// The module's own functions and actions this one calls.
    pub calls: BTreeSet<usize>,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub kind: FuncKind,
    pub name: String,
    /// The export name (`a_<id>`, `f_<id>`, `v_<id>`, `n_<id>`, `b_main`).
    pub export: String,
    pub nparams: usize,
    pub result: Ty,
    /// Types of all locals, parameters first.
    pub locals: Vec<Ty>,
    pub body: Vec<Stmt>,
    pub facts: Facts,
    /// Where the declaration it was lowered from starts, in the package source's span space
    /// (see [`crate::diag::Placed`]): the line a profile names it by. None for a function no
    /// declaration is written for, and for a block.
    pub at: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub funcs: Vec<Func>,
    /// The views the checker made of UI loop bodies, after the declared functions in `funcs`.
    pub views: Vec<SynthView>,
    /// The view state templates declare where they expand (`std.list`'s query), by the id of
    /// the view they expand in.
    pub states: Vec<(String, crate::resolve::StateInfo)>,
    /// The arguments of actions views apply that compute from view state, after the row views
    /// in `funcs`: the runtime runs each when the control's event fires.
    pub at_event: Vec<AtEventFn>,
    /// What each view does when the window is asked to show an item (`on show(x: T): ...`), each
    /// a function returning the action to apply, after the arguments above.
    pub shows: Vec<ViewShow>,
    /// The words the views show as they wrote them: each a message of the package's catalogue.
    pub words: Vec<crate::meta::WordMeta>,
}

/// An argument of an action a view applies that computes from view state (`set(open, not
/// open)`, `send(c, "{draft}!")`), as a function of its own: it takes the view state and the
/// values it reads, and the runtime runs it when the event fires, on the view state as it is
/// then. Exported as `e_<id>` ([`at_event_export`]).
#[derive(Clone, Debug)]
pub struct AtEventFn {
    /// Index of its function in [`Module::funcs`].
    pub func: usize,
    pub id: String,
    pub name: String,
    /// The view state it reads and the values from around it, in the order the view passes
    /// them.
    pub params: Vec<crate::resolve::ParamInfo>,
    pub result: Ty,
}

/// What a view answers when the window is asked to show an item of `ty`: `on show(x: T): ...`
/// in its body. The runtime runs it with the item, and applies the action it returns, in the
/// view's own state. Exported as `s_<view id>` ([`show_export`]).
#[derive(Clone, Debug)]
pub struct ViewShow {
    /// The id of the view that answers.
    pub view: String,
    /// The id of the declared type of item it answers for: what a resource reference carries.
    pub ty: String,
    /// The export of its function ([`show_export`]).
    pub export: String,
    /// The item it is run with.
    pub param: crate::resolve::ParamInfo,
    /// The index of its function in [`Module::funcs`].
    pub func: usize,
}

/// The export of the function a view answers a "show this item" with, by the view's id.
pub fn show_export(view: &str) -> String {
    format!("s_{view}")
}

/// The export of the argument function with id `id` ([`AtEventFn`]).
pub fn at_event_export(id: &str) -> String {
    format!("e_{id}")
}

/// The export of the guard of the action with id `id`: the action's requirements its subject
/// alone decides, answering the first reason it cannot run on the subject now, or the empty
/// text while it can ([`FuncKind::Guard`]).
pub fn guard_export(id: &str) -> String {
    format!("q_{id}")
}

/// The export of the `applies` conditions of the action with id `id`: the action's conditions
/// its subject and the place it is offered in alone decide, answering the first reason it cannot
/// apply to the item where it stands now, or the empty text while it can
/// ([`FuncKind::Applies`]).
pub fn applies_export(id: &str) -> String {
    format!("p_{id}")
}

/// A view the checker made of the body of a UI `for`: one boundary per item, keyed by it, which
/// the host renders when it shows the row.
#[derive(Clone, Debug)]
pub struct SynthView {
    /// Index of its function in [`Module::funcs`].
    pub func: usize,
    pub id: String,
    pub name: String,
    /// The id of the declared view the loop was written in.
    pub owner: String,
    /// The loop's item first, then each value the body uses from around the loop.
    pub params: Vec<crate::resolve::ParamInfo>,
}

impl Expr {
    pub const fn new(kind: Ek, ty: Ty) -> Self {
        Self { kind, ty }
    }

    pub const fn error() -> Self {
        Self {
            kind: Ek::Int(0),
            ty: Ty::Error,
        }
    }

    pub fn host(imp: Imp, args: Vec<Self>, ty: Ty) -> Self {
        Self::new(Ek::Host(imp, args), ty)
    }

    pub fn str(s: &str) -> Self {
        Self::new(Ek::Str(s.to_string()), Ty::Text)
    }

    pub fn local(id: LocalId, ty: Ty) -> Self {
        Self::new(Ek::Local(id), ty)
    }

    /// The same value seen as another type of the same representation (a `Date` is text).
    #[must_use]
    pub fn typed(self, ty: Ty) -> Self {
        Self { ty, ..self }
    }

    pub fn int(v: i64) -> Self {
        Self::new(Ek::Int(v), Ty::Int)
    }
}
