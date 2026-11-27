//! The typed IR between the checker and the wasm backend. Structured control flow, explicit
//! host calls, no names.

use crate::abi::Imp;
use crate::types::{EffectClass, Ty};
use std::collections::BTreeSet;

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
    /// `10s`: a length of time as its seconds, only where one is a value (the moment an outside
    /// effect is held until).
    Duration(i64),
    Bool(bool),
    I32(i32),
    Str(String),
    Local(LocalId),
    /// A host import call. The expression's type says whether the result is kept.
    Host(Imp, Vec<Expr>),
    /// A call of this module's function, by index into [`Module::funcs`].
    Call(usize, Vec<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    Seq(Vec<Stmt>, Box<Expr>),
    Conv(Conv, Box<Expr>),
    Unit,
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
    /// An action's requirements its target alone decides: the first reason it cannot run on the
    /// target now, or empty while it can ([`guard_export`]).
    Guard,
    /// An action's `applies` conditions its target and place decide: the first reason it cannot
    /// apply where it is offered, or empty while it can ([`applies_export`]).
    Applies,
}

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

#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one thing the compiler read of a body on its own: whether it changes \
              anything, reaches outside, tells a person, reads an earlier build, or invokes an \
              action"
)]
pub struct Facts {
    pub mutates: bool,
    pub outside: bool,
    /// What the body does leaves the system for good: it calls a capability that does
    /// ([`crate::origin::LEAVING`]), or asks an op `leaves: true`.
    pub leaves: bool,
    /// The capabilities the body's `outside(...)` calls name (`youtube.search`), as written: what
    /// the action is tagged with ([`crate::meta::ActionMeta::outside`]).
    pub outside_caps: BTreeSet<String>,
    /// The body calls `notify(...)`: it tells a person something, writing the host's `Notice`
    /// ([`crate::meta::ActionMeta::notice`]).
    pub notice: bool,
    pub invokes: BTreeSet<ActionRef>,
    pub action_values: BTreeSet<ActionRef>,
    pub views: BTreeSet<usize>,
    /// The effects the body's own code reads; what it reaches through a call is
    /// [`crate::effects::reached`]'s.
    pub effects: BTreeSet<crate::effects::Effect>,
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
    /// Where the declaration it was lowered from starts, in the package source's span space: the
    /// line a profile names it by. `None` for a block or a function no declaration is written for.
    pub at: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub funcs: Vec<Func>,
    /// The views the checker made of UI loop bodies, after the declared functions in `funcs`.
    pub views: Vec<SynthView>,
    /// The view state templates declare where they expand (`std.list`'s query), by the id of the
    /// view they expand in.
    pub states: Vec<(String, crate::resolve::StateInfo)>,
    /// The arguments of actions views apply that compute from view state, run when the control's
    /// event fires.
    pub at_event: Vec<AtEventFn>,
    /// What each view does when the window is asked to show an item: a function returning the
    /// action to apply.
    pub shows: Vec<ViewShow>,
    /// The places each view's window can be opened at: a function returning the action that
    /// takes the window there.
    pub places: Vec<ViewPlace>,
    pub words: Vec<crate::meta::WordMeta>,
    /// The views a capture or a stream leaves out, by id: those that declare `protect capture`,
    /// those a protected template expanded in, and those showing one of either.
    pub capture_protected: Vec<String>,
    /// Each call of one view from another, the caller first: the nested views that stand in a
    /// view's frame.
    pub view_calls: Vec<(String, String)>,
}

/// An argument of an action a view applies that computes from view state (`set(open, not open)`),
/// as a function of its own: it takes the view state and the values it reads. Exported as `e_<id>`
/// ([`at_event_export`]).
#[derive(Clone, Debug)]
pub struct AtEventFn {
    pub func: usize,
    pub id: String,
    pub name: String,
    /// The view state it reads and the values from around it, in the order the view passes
    /// them.
    pub params: Vec<crate::resolve::ParamInfo>,
    pub result: Ty,
}

/// What a view answers when the window is asked to show an item of `ty` (`on show(x: T)` in its
/// body): the runtime runs it with the item and applies the action it returns. Exported as
/// `s_<view id>_<type id>` ([`show_export`]).
#[derive(Clone, Debug)]
pub struct ViewShow {
    pub view: String,
    pub ty: String,
    pub export: String,
    pub param: crate::resolve::ParamInfo,
    pub func: usize,
}

/// A place a view's window can be opened at (`on show week: set(place, "week")` in its body):
/// the runtime runs the function and applies the action it returns in the view's state, as it
/// does an item's [`ViewShow`]. Exported as `p_<place id>` ([`place_export`]).
#[derive(Clone, Debug)]
pub struct ViewPlace {
    pub view: String,
    /// Its name in the source: what a caller opens it by.
    pub name: String,
    /// Its id in `app.ids`, under which its words are translated.
    pub id: String,
    /// Its words, in the package's own language: the label written, or its name in sentence case.
    pub label: String,
    pub export: String,
    pub func: usize,
}

/// The export of the function that takes a window to the place with id `id`.
pub fn place_export(id: &str) -> String {
    format!("p_{id}")
}

/// The export of the function a view answers a "show this item" of the type `ty` with: the
/// view's id and the type's, since a view may answer for more than one type.
pub fn show_export(view: &str, ty: &str) -> String {
    format!("s_{view}_{ty}")
}

pub fn at_event_export(id: &str) -> String {
    format!("e_{id}")
}

/// The export of the guard of the action with id `id` ([`FuncKind::Guard`]).
pub fn guard_export(id: &str) -> String {
    format!("q_{id}")
}

/// The export of the `applies` conditions of the action with id `id` ([`FuncKind::Applies`]).
pub fn applies_export(id: &str) -> String {
    format!("p_{id}")
}

/// A view the checker made of the body of a UI `for`: one boundary per item, keyed by it.
#[derive(Clone, Debug)]
pub struct SynthView {
    pub func: usize,
    pub id: String,
    /// The id of the declared view the loop was written in.
    pub owner: String,
    /// The loop's item first, then each value the body uses from around the loop.
    pub params: Vec<crate::resolve::ParamInfo>,
}

impl Expr {
    pub const fn new(kind: Ek, ty: Ty) -> Self {
        Self { kind, ty }
    }

    /// What stands where checking found no value: a diagnostic names it, or a template's type
    /// parameter no argument bound (a defaulted `Option<M>` left `.none`), whose code then never
    /// runs but still has to validate. It is held as `Ty::Error` is, an `i32` handle, so the
    /// module around it type-checks either way.
    pub const fn error() -> Self {
        Self {
            kind: Ek::I32(0),
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

    #[must_use]
    pub fn typed(self, ty: Ty) -> Self {
        Self { ty, ..self }
    }

    pub fn int(v: i64) -> Self {
        Self::new(Ek::Int(v), Ty::Int)
    }
}
