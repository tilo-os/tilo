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
    Flow,
    View,
    Block,
}

/// An action or flow that code can apply: `app` and `id` name it for the host.
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
    pub inline_actions: BTreeSet<usize>,
    pub invokes: BTreeSet<ActionRef>,
    pub action_values: BTreeSet<ActionRef>,
    pub views: BTreeSet<usize>,
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

/// The export of the argument function with id `id` ([`AtEventFn`]).
pub fn at_event_export(id: &str) -> String {
    format!("e_{id}")
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
