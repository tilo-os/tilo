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

    pub fn int(v: i64) -> Self {
        Self::new(Ek::Int(v), Ty::Int)
    }
}
