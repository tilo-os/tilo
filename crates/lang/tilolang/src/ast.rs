//! The syntax tree. Every declaration, field, parameter and UI call carries a [`NodeId`], and
//! every node carries its span.

use crate::diag::Span;
use crate::types::MergeKind;

/// Identity of a syntax node within one parsed module.
pub type NodeId = u32;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub app: AppHeader,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppHeader {
    pub id: String,
    pub title: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Use(UseDecl),
    Type(TypeDecl),
    Action(ActionDecl),
    Flow(FlowDecl),
    View(ViewDecl),
    Fn(FnDecl),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UseDecl {
    pub node: NodeId,
    pub app: String,
    pub alias: Option<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub fields: Vec<FieldDecl>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MergeSpec {
    pub kind: MergeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub ty: TypeExpr,
    pub merge: Option<MergeSpec>,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeExprKind {
    /// `Int`, `Album`, `todo.Item`.
    Named(Vec<Ident>),
    /// `List<T>`, `Collection<T>`, ...
    Generic(Ident, Vec<TypeExpr>),
    Tuple(Vec<TypeExpr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub node: NodeId,
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub view: Ident,
    /// The action parameters passed to the view, when written out.
    pub args: Option<Vec<Ident>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActionDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub result: Option<TypeExpr>,
    pub proposal: Option<Proposal>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub result: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub body: Vec<UiStmt>,
    pub body_span: Span,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub node: NodeId,
    pub doc: Vec<String>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub result: TypeExpr,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
}

impl AssignOp {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Set => "=",
            Self::Add => "+=",
            Self::Sub => "-=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub segs: Vec<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Else {
    Block(Block),
    If(Box<Stmt>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Let {
        name: Ident,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        place: Place,
        op: AssignOp,
        value: Expr,
    },
    If {
        cond: Expr,
        then: Block,
        els: Option<Else>,
    },
    For {
        var: Ident,
        iter: Expr,
        body: Block,
    },
    Return(Option<Expr>),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl BinOp {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
        }
    }

    /// Binding strength, higher binds tighter.
    pub const fn prec(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::And => 2,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge => 4,
            Self::Add | Self::Sub => 5,
            Self::Mul | Self::Div | Self::Rem => 6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefLit {
    /// The app part as written (an app id or an alias); empty for `@kind:id`.
    pub app: String,
    pub ty: String,
    pub id: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    Interp(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    None,
    Str(Vec<StrPart>),
    Ref(RefLit),
    Ident(String),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    IfExpr {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Lambda {
        param: Ident,
        body: Box<Expr>,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: Ident,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Arg>,
    },
    Do(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiStmt {
    pub kind: UiKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiKind {
    Call(UiCall),
    For {
        var: Ident,
        iter: Expr,
        body: Vec<UiStmt>,
    },
    If {
        cond: Expr,
        then: Vec<UiStmt>,
        els: Option<Vec<UiStmt>>,
    },
    Let {
        name: Ident,
        value: Expr,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiCall {
    pub node: NodeId,
    /// A call of a UI primitive or of a view, or a bare name.
    pub call: Expr,
    pub block: Option<Vec<UiStmt>>,
    pub anchor: Option<Ident>,
}
